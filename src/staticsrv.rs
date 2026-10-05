use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cap_std::fs::Dir;

use crate::ports;

const WORKERS: usize = 4;
const QUEUE_CAPACITY: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const STREAM_CHUNK: usize = 64 * 1024;

pub struct StaticServer {
    root: Arc<Dir>,
    listener: TcpListener,
}

impl StaticServer {
    pub fn bind(root: &Path, start: u16) -> Result<Self> {
        let meta =
            std::fs::metadata(root).with_context(|| format!("cannot read {}", root.display()))?;
        if !meta.is_dir() {
            bail!("{} is not a directory", root.display());
        }
        let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .with_context(|| format!("cannot open {}", root.display()))?;
        let listener = ports::reserve(start)?;
        Ok(Self {
            root: Arc::new(root),
            listener,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn serve(self, stop: Arc<AtomicBool>) -> Result<()> {
        self.listener.set_nonblocking(true)?;
        let port = self.local_addr()?.port();
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(QUEUE_CAPACITY);

        thread::scope(move |scope| {
            let rx = Arc::new(Mutex::new(rx));
            let mut workers = Vec::with_capacity(WORKERS);
            for _ in 0..WORKERS {
                let spawned = thread::Builder::new()
                    .name("srvm-static".into())
                    .spawn_scoped(scope, {
                        let rx = Arc::clone(&rx);
                        let root = Arc::clone(&self.root);
                        let stop = Arc::clone(&stop);
                        move || worker_loop(rx, root, port, stop)
                    });
                match spawned {
                    Ok(handle) => workers.push(handle),
                    Err(err) => {
                        stop.store(true, Ordering::SeqCst);
                        drop(tx);
                        return Err(err).context("failed to spawn static worker");
                    }
                }
            }
            if let Err(err) = accept_loop(&self.listener, &tx, &stop) {
                stop.store(true, Ordering::SeqCst);
                return Err(err);
            }
            Ok(())
        })
    }
}

fn accept_loop(
    listener: &TcpListener,
    tx: &mpsc::SyncSender<TcpStream>,
    stop: &AtomicBool,
) -> Result<()> {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit non-blocking mode on Windows and
                // BSD/macOS; blocking mode is required for read/write timeouts.
                let _ = stream.set_nonblocking(false);
                match tx.try_send(stream) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err).context("static accept failed"),
        }
    }
    Ok(())
}

fn worker_loop(
    rx: Arc<Mutex<mpsc::Receiver<TcpStream>>>,
    root: Arc<Dir>,
    port: u16,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let stream = {
            let rx = rx.lock().expect("queue lock poisoned");
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(stream) => stream,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        let _ = handle_connection(stream, &root, port, &stop);
    }
}

struct RequestInfo {
    method: String,
    target: String,
    host: Option<String>,
}

enum RequestError {
    Closed,
    Status { code: u16, head: bool },
}

fn handle_connection(
    mut stream: TcpStream,
    root: &Dir,
    port: u16,
    stop: &AtomicBool,
) -> io::Result<()> {
    let request = match read_request(&mut stream, stop) {
        Ok(request) => request,
        Err(RequestError::Closed) => return Ok(()),
        Err(RequestError::Status { code, head }) => {
            return respond_error(&mut stream, code, head, "", stop);
        }
    };

    let head = match request.method.as_str() {
        "GET" => false,
        "HEAD" => true,
        _ => {
            return respond_error(&mut stream, 405, false, "Allow: GET, HEAD\r\n", stop);
        }
    };

    let Some(host) = request.host.as_deref() else {
        return respond_error(&mut stream, 400, head, "", stop);
    };
    if !host_allowed(host, port) {
        return respond_error(&mut stream, 403, head, "", stop);
    }

    let (rel, had_slash) = match decode_target(&request.target) {
        Ok(pair) => pair,
        Err(code) => return respond_error(&mut stream, code, head, "", stop),
    };

    serve_path(
        &mut stream,
        root,
        &request.target,
        &rel,
        had_slash,
        head,
        stop,
    )
}

fn read_request(
    stream: &mut TcpStream,
    stop: &AtomicBool,
) -> std::result::Result<RequestInfo, RequestError> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        {
            let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
            let mut request = httparse::Request::new(&mut headers);
            match request.parse(&buf) {
                Ok(httparse::Status::Complete(_)) => {
                    let method = request.method.unwrap_or_default().to_string();
                    let target = request.path.unwrap_or_default().to_string();
                    let mut host: Option<String> = None;
                    let mut duplicate = false;
                    for header in request.headers.iter() {
                        if header.name.eq_ignore_ascii_case("host") {
                            duplicate = host.is_some();
                            host = Some(String::from_utf8_lossy(header.value).into_owned());
                        }
                    }
                    if duplicate {
                        return Err(RequestError::Status {
                            code: 400,
                            head: method == "HEAD",
                        });
                    }
                    return Ok(RequestInfo {
                        method,
                        target,
                        host,
                    });
                }
                Ok(httparse::Status::Partial) => {}
                Err(httparse::Error::TooManyHeaders) => {
                    return Err(RequestError::Status {
                        code: 431,
                        head: head_request(&buf),
                    });
                }
                Err(_) => {
                    return Err(RequestError::Status {
                        code: 400,
                        head: head_request(&buf),
                    });
                }
            }
        }

        if buf.len() >= MAX_HEADER_BYTES {
            return Err(RequestError::Status {
                code: 431,
                head: head_request(&buf),
            });
        }
        if stop.load(Ordering::SeqCst) {
            return Err(RequestError::Closed);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(RequestError::Status {
                code: 408,
                head: head_request(&buf),
            });
        };
        stream
            .set_read_timeout(Some(remaining.min(Duration::from_millis(200))))
            .map_err(|_| RequestError::Closed)?;
        let mut chunk = [0u8; 4096];
        let limit = (MAX_HEADER_BYTES - buf.len()).min(chunk.len());
        match stream.read(&mut chunk[..limit]) {
            Ok(0) => {
                return Err(if buf.is_empty() {
                    RequestError::Closed
                } else {
                    RequestError::Status {
                        code: 400,
                        head: head_request(&buf),
                    }
                });
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                if stop.load(Ordering::SeqCst) {
                    return Err(RequestError::Closed);
                }
            }
            Err(_) => return Err(RequestError::Closed),
        }
    }
}

fn head_request(buf: &[u8]) -> bool {
    buf.starts_with(b"HEAD ")
}

fn host_allowed(host: &str, port: u16) -> bool {
    let lower = host.to_ascii_lowercase();
    let (name, port_part) = match lower.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (lower.as_str(), None),
    };
    if name != "127.0.0.1" && name != "localhost" {
        return false;
    }
    match port_part {
        None => port == 80,
        Some(raw) => raw.parse::<u16>() == Ok(port),
    }
}

fn decode_target(target: &str) -> std::result::Result<(PathBuf, bool), u16> {
    let raw_path = target.split_once('?').map(|(p, _)| p).unwrap_or(target);
    if !raw_path.starts_with('/')
        || raw_path.starts_with("//")
        || raw_path.contains('#')
        || raw_path.contains('\\')
    {
        return Err(400);
    }

    let decoded = percent_decode(raw_path)?;
    let decoded = String::from_utf8(decoded).map_err(|_| 400u16)?;
    if decoded.starts_with("//")
        || decoded
            .chars()
            .any(|ch| ch.is_control() || ch == '\\' || ch == ':')
    {
        return Err(400);
    }

    let mut path = PathBuf::new();
    for comp in decoded.split('/') {
        if comp.is_empty() {
            continue;
        }
        match comp {
            "." | ".." => return Err(403),
            c if c.starts_with('.') => return Err(404),
            c if c.ends_with('.') || c.ends_with(' ') => return Err(404),
            c if is_device_stem(c) => return Err(404),
            c => {
                for piece in Path::new(c).components() {
                    match piece {
                        Component::Normal(os) => path.push(os),
                        _ => return Err(400),
                    }
                }
            }
        }
    }

    Ok((path, decoded.ends_with('/')))
}

fn percent_decode(raw: &str) -> std::result::Result<Vec<u8>, u16> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if bytes.len() - i < 3 {
                    return Err(400);
                }
                let hi = hex_value(bytes[i + 1]).ok_or(400u16)?;
                let lo = hex_value(bytes[i + 2]).ok_or(400u16)?;
                out.push(hi << 4 | lo);
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_device_stem(comp: &str) -> bool {
    let stem = comp.split('.').next().unwrap_or("");
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    if let Some(rest) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    {
        return rest.len() == 1 && matches!(rest.as_bytes()[0], b'1'..=b'9');
    }
    false
}

fn serve_path(
    stream: &mut TcpStream,
    root: &Dir,
    target: &str,
    rel: &Path,
    had_slash: bool,
    head: bool,
    stop: &AtomicBool,
) -> io::Result<()> {
    let (raw_path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
    let mut prefix = PathBuf::new();
    let mut final_meta = None;
    for comp in rel.components() {
        prefix.push(comp);
        match root.symlink_metadata(&prefix) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return respond_error(stream, 404, head, "", stop);
                }
                final_meta = Some(meta);
            }
            Err(_) => return respond_error(stream, 404, head, "", stop),
        }
    }

    let meta = match (rel.as_os_str().is_empty(), final_meta) {
        (true, _) => match root.metadata(".") {
            Ok(meta) => meta,
            Err(_) => return respond_error(stream, 404, head, "", stop),
        },
        (false, Some(meta)) => meta,
        (false, None) => return respond_error(stream, 404, head, "", stop),
    };

    let file_path = if meta.is_dir() {
        let index = rel.join("index.html");
        let has_index = matches!(
            root.symlink_metadata(&index),
            Ok(meta) if !meta.file_type().is_symlink() && meta.file_type().is_file()
        );
        if !has_index {
            return respond_error(stream, 404, head, "", stop);
        }
        if !had_slash && !rel.as_os_str().is_empty() {
            let mut location = format!("{raw_path}/");
            if let Some(query) = query {
                location.push('?');
                location.push_str(query);
            }
            let extra = format!("Location: {location}\r\n");
            return respond(stream, head, 308, None, &extra, Body::Empty, stop);
        }
        index
    } else if meta.is_file() {
        if had_slash {
            return respond_error(stream, 404, head, "", stop);
        }
        rel.to_path_buf()
    } else {
        return respond_error(stream, 404, head, "", stop);
    };

    let file = match root.open_with(&file_path, &open_options()) {
        Ok(file) => file,
        Err(_) => return respond_error(stream, 404, head, "", stop),
    };
    let meta = match file.metadata() {
        Ok(meta) if meta.file_type().is_file() => meta,
        _ => return respond_error(stream, 404, head, "", stop),
    };

    respond(
        stream,
        head,
        200,
        Some(mime_type(&file_path)),
        "",
        Body::File(file, meta.len()),
        stop,
    )
}

#[cfg(unix)]
fn open_options() -> cap_std::fs::OpenOptions {
    use cap_std::fs::OpenOptionsExt;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK);
    options
}

#[cfg(not(unix))]
fn open_options() -> cap_std::fs::OpenOptions {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    options
}

fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("webmanifest") => "application/manifest+json",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

enum Body {
    Empty,
    Text(String),
    File(cap_std::fs::File, u64),
}

fn respond_error(
    stream: &mut TcpStream,
    code: u16,
    head: bool,
    extra: &str,
    stop: &AtomicBool,
) -> io::Result<()> {
    let body = format!("{code} {}\n", reason_phrase(code));
    respond(
        stream,
        head,
        code,
        Some("text/plain"),
        extra,
        Body::Text(body),
        stop,
    )
}

fn respond(
    stream: &mut TcpStream,
    head: bool,
    code: u16,
    content_type: Option<&str>,
    extra: &str,
    body: Body,
    stop: &AtomicBool,
) -> io::Result<()> {
    let len = match &body {
        Body::Empty => 0,
        Body::Text(text) => text.len() as u64,
        Body::File(_, len) => *len,
    };

    let mut out = format!(
        "HTTP/1.1 {code} {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{extra}Content-Length: {len}\r\n",
        reason_phrase(code)
    );
    if let Some(content_type) = content_type {
        out.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    out.push_str("\r\n");

    let deadline = Instant::now() + RESPONSE_DEADLINE;
    write_all_bounded(stream, out.as_bytes(), deadline, stop)?;
    if head {
        return Ok(());
    }
    match body {
        Body::Empty => Ok(()),
        Body::Text(text) => write_all_bounded(stream, text.as_bytes(), deadline, stop),
        Body::File(mut file, len) => stream_file(stream, &mut file, len, deadline, stop),
    }
}

fn stream_file(
    stream: &mut TcpStream,
    file: &mut cap_std::fs::File,
    mut remaining: u64,
    deadline: Instant,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut buf = [0u8; STREAM_CHUNK];
    while remaining > 0 {
        if stop.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        let want = remaining.min(buf.len() as u64) as usize;
        match file.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                write_all_bounded(stream, &buf[..n], deadline, stop)?;
                remaining -= n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn write_all_bounded(
    stream: &mut TcpStream,
    mut buf: &[u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> io::Result<()> {
    while !buf.is_empty() {
        if stop.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "response deadline"));
        }
        stream.set_write_timeout(Some(remaining.min(WRITE_TIMEOUT)))?;
        match stream.write(buf) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "closed")),
            Ok(n) => buf = &buf[n..],
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_target, host_allowed, is_device_stem, mime_type};
    use std::path::{Path, PathBuf};

    #[test]
    fn decode_accepts_plain_and_encoded_paths() {
        assert_eq!(decode_target("/").unwrap(), (PathBuf::new(), true));
        assert_eq!(
            decode_target("/a/b.txt?x=1").unwrap(),
            (PathBuf::from("a/b.txt"), false)
        );
        assert_eq!(
            decode_target("/a%20b/caf%C3%A9.txt").unwrap(),
            (PathBuf::from("a b/café.txt"), false)
        );
        assert_eq!(
            decode_target("/docs/?y=2").unwrap(),
            (PathBuf::from("docs"), true)
        );
        assert_eq!(
            decode_target("/a+b.txt").unwrap().0,
            PathBuf::from("a+b.txt")
        );
        assert_eq!(
            decode_target("/docs%2f").unwrap(),
            (PathBuf::from("docs"), true)
        );
        assert_eq!(
            decode_target("/a%2Fb.txt").unwrap(),
            (PathBuf::from("a/b.txt"), false)
        );
    }

    #[test]
    fn decode_rejects_traversal_and_bad_input() {
        for (target, code) in [
            ("../outside", 400),
            ("//evil", 400),
            ("/a#b", 400),
            ("/a\\b", 400),
            ("/%2e%2e/out", 403),
            ("/..%2fout", 403),
            ("/a/./b", 403),
            ("/a/../b", 403),
            ("/%5cb", 400),
            ("/%00x", 400),
            ("/%FFx", 400),
            ("/%zz", 400),
            ("/%2", 400),
            ("/%2fetc", 400),
            ("/%2f", 400),
            ("/C:%5cwin", 400),
            ("/.env", 404),
            ("/.git/config", 404),
            ("/foo./x", 404),
            ("/foo /x", 404),
            ("/con.txt", 404),
            ("/COM1", 404),
            ("/lpt9.png", 404),
        ] {
            assert_eq!(decode_target(target), Err(code), "{target}");
        }
    }

    #[test]
    fn device_stems_cover_windows_reserved_names() {
        for stem in ["con", "CON", "prn.txt", "aux", "NUL.md", "com3", "LPT1"] {
            assert!(is_device_stem(stem), "{stem}");
        }
        for stem in ["confoo", "com10", "lpt", "normal.txt"] {
            assert!(!is_device_stem(stem), "{stem}");
        }
    }

    #[test]
    fn host_header_must_match_bound_loopback_port() {
        assert!(host_allowed("127.0.0.1:8080", 8080));
        assert!(host_allowed("LOCALHOST:8080", 8080));
        assert!(host_allowed("localhost:8080", 8080));
        assert!(host_allowed("127.0.0.1", 80));

        assert!(!host_allowed("127.0.0.1:8081", 8080));
        assert!(!host_allowed("127.0.0.1", 8080));
        assert!(!host_allowed("evil.com:8080", 8080));
        assert!(!host_allowed("example.com", 80));
        assert!(!host_allowed("::1:8080", 8080));
        assert!(!host_allowed("[::1]:8080", 8080));
        assert!(!host_allowed("127.0.0.1:0x1f90", 8080));
    }

    #[test]
    fn mime_table_covers_common_assets() {
        assert_eq!(mime_type(Path::new("a.html")), "text/html; charset=utf-8");
        assert_eq!(mime_type(Path::new("a.HTM")), "text/html; charset=utf-8");
        assert_eq!(mime_type(Path::new("a.CSS")), "text/css; charset=utf-8");
        assert_eq!(
            mime_type(Path::new("a.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(mime_type(Path::new("a.bin")), "application/octet-stream");
        assert_eq!(mime_type(Path::new("noext")), "application/octet-stream");
    }
}
