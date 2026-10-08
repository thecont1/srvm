use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

mod support;

#[cfg(unix)]
use std::process::ExitStatus;

use srvm::staticsrv::StaticServer;
use tempfile::{TempDir, tempdir};

struct Response {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Response {
    fn header_value(&self, name: &str) -> Option<String> {
        let needle = format!("{}:", name.to_ascii_lowercase());
        self.headers.lines().find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .starts_with(&needle)
                .then(|| line[needle.len()..].trim().to_string())
        })
    }
}

struct TestServer {
    stop: Arc<AtomicBool>,
    port: u16,
    handle: Option<thread::JoinHandle<anyhow::Result<()>>>,
}

impl TestServer {
    fn start(root: &Path) -> Self {
        let server = StaticServer::bind(root, 0).unwrap();
        let port = server.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::spawn(move || server.serve(flag));
        Self {
            stop,
            port,
            handle: Some(handle),
        }
    }

    fn stop_and_join(&mut self, timeout: Duration) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self.handle.take().unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            if handle.is_finished() {
                handle.join().unwrap().unwrap();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server did not stop in {timeout:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

fn html_root(files: &[(&str, &[u8])]) -> TempDir {
    let root = tempdir().unwrap();
    for (rel, content) in files {
        let path = root.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    root
}

fn connect(port: u16) -> TcpStream {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

fn exchange(port: u16, request: &str) -> Response {
    let mut stream = connect(port);
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut raw = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                assert!(Instant::now() < deadline, "timed out reading response");
            }
            Err(_) => break,
        }
    }
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Response {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let headers = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Response {
        status,
        headers,
        body: raw.get(split + 4..).unwrap_or(&[]).to_vec(),
    }
}

fn get(port: u16, target: &str) -> Response {
    exchange(
        port,
        &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

fn head(port: u16, target: &str) -> Response {
    exchange(
        port,
        &format!("HEAD {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

#[test]
fn bound_port_is_reserved_before_serve() {
    let root = html_root(&[("index.html", b"hello")]);
    let server = StaticServer::bind(root.path(), 0).unwrap();
    let port = server.local_addr().unwrap().port();

    assert!(
        TcpListener::bind(("127.0.0.1", port)).is_err(),
        "reserved port must not be rebindable before serve"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = thread::spawn(move || server.serve(flag));

    let response = get(port, "/");
    assert_eq!(response.status, 200);

    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn get_and_head_index() {
    let root = html_root(&[("index.html", b"<h1>hi</h1>\n")]);
    let server = TestServer::start(root.path());

    let response = get(server.port, "/");
    assert_eq!(response.status, 200, "{}", response.headers);
    assert_eq!(response.body, b"<h1>hi</h1>\n");
    assert_eq!(
        response.header_value("Content-Type").as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        response.header_value("Content-Length").as_deref(),
        Some("12")
    );
    assert_eq!(
        response.header_value("Cache-Control").as_deref(),
        Some("no-store")
    );
    assert_eq!(
        response.header_value("Connection").as_deref(),
        Some("close")
    );

    let head = head(server.port, "/");
    assert_eq!(head.status, 200, "{}", head.headers);
    assert_eq!(head.header_value("Content-Length").as_deref(), Some("12"));
    assert_eq!(
        head.header_value("Content-Type").as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert!(head.body.is_empty());
}

#[test]
fn binary_and_mime_types() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
    let root = html_root(&[
        ("index.html", b"ok"),
        ("img.png", png),
        ("app.js", b"console.log(1)"),
        ("style.css", b"body{}"),
        ("data.json", b"{}"),
        ("pkg.wasm", b"\x00asm"),
        ("blob.bin", b"\x00\xff\x01"),
    ]);
    let server = TestServer::start(root.path());

    let png_resp = get(server.port, "/img.png");
    assert_eq!(png_resp.status, 200);
    assert_eq!(png_resp.body, png, "binary bytes incl NUL must be exact");
    assert_eq!(
        png_resp.header_value("Content-Type").as_deref(),
        Some("image/png")
    );

    for (target, mime) in [
        ("/app.js", "text/javascript; charset=utf-8"),
        ("/style.css", "text/css; charset=utf-8"),
        ("/data.json", "application/json"),
        ("/pkg.wasm", "application/wasm"),
        ("/blob.bin", "application/octet-stream"),
    ] {
        let response = get(server.port, target);
        assert_eq!(response.status, 200, "{target}");
        assert_eq!(
            response.header_value("Content-Type").as_deref(),
            Some(mime),
            "{target}"
        );
    }
}

#[test]
fn nested_encoded_and_query_paths() {
    let root = html_root(&[
        ("index.html", b"root"),
        ("sub/dir/asset.css", b"nested"),
        ("a b.txt", b"spaced"),
        ("caf\u{e9}.txt", "café".as_bytes()),
    ]);
    let server = TestServer::start(root.path());

    assert_eq!(get(server.port, "/sub/dir/asset.css").body, b"nested");
    assert_eq!(get(server.port, "/a%20b.txt").body, b"spaced");
    assert_eq!(get(server.port, "/caf%C3%A9.txt").body, "café".as_bytes());
    assert_eq!(get(server.port, "/index.html?x=1&y=2").body, b"root");
    assert_eq!(get(server.port, "/a%20b.txt?reload=1").body, b"spaced");
}

#[test]
fn fresh_bytes_after_edit() {
    let root = html_root(&[("index.html", b"first")]);
    let server = TestServer::start(root.path());
    assert_eq!(get(server.port, "/").body, b"first");

    fs::write(root.path().join("index.html"), b"second").unwrap();
    assert_eq!(get(server.port, "/").body, b"second");
}

#[test]
fn large_file_streams_exactly() {
    let mut payload = Vec::with_capacity(200_000);
    while payload.len() < 200_000 {
        payload.extend_from_slice(b"srvm-static-stream\n");
    }
    payload.truncate(200_000);
    let root = html_root(&[("index.html", b"ok")]);
    fs::write(root.path().join("big.bin"), &payload).unwrap();
    let server = TestServer::start(root.path());

    let response = get(server.port, "/big.bin");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header_value("Content-Length").as_deref(),
        Some("200000")
    );
    assert_eq!(response.body, payload, "streamed bytes must match disk");

    let head = head(server.port, "/big.bin");
    assert_eq!(head.status, 200);
    assert_eq!(
        head.header_value("Content-Length").as_deref(),
        Some("200000")
    );
    assert!(head.body.is_empty());
}

#[test]
fn range_requests_get_full_200_body() {
    let root = html_root(&[("index.html", b"ok"), ("data.txt", b"0123456789")]);
    let server = TestServer::start(root.path());

    for range in ["bytes=0-3", "bytes=4-", "bytes=-5", "bytes=0-0,2-2"] {
        let response = exchange(
            server.port,
            &format!(
                "GET /data.txt HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nRange: {range}\r\n\r\n",
                server.port
            ),
        );
        assert_eq!(response.status, 200, "{range}");
        assert_eq!(response.body, b"0123456789", "{range}");
        assert!(response.header_value("Content-Range").is_none(), "{range}");
        assert!(response.header_value("Accept-Ranges").is_none(), "{range}");
    }
}

#[test]
fn stop_joins_and_releases_port() {
    let root = html_root(&[("index.html", b"x")]);
    let mut server = TestServer::start(root.path());
    let port = server.port;

    let began = Instant::now();
    server.stop_and_join(Duration::from_secs(3));
    assert!(began.elapsed() < Duration::from_secs(3));

    support::bind_retrying(port, 25, Duration::from_millis(200))
        .expect("port must be rebindable after stop");
}

#[test]
fn directory_index_and_redirects() {
    let root = html_root(&[
        ("index.html", b"root"),
        ("docs/index.html", b"docs index"),
        ("file.txt", b"file"),
    ]);
    fs::create_dir(root.path().join("emptydir")).unwrap();
    let server = TestServer::start(root.path());

    let docs = get(server.port, "/docs/");
    assert_eq!(docs.status, 200);
    assert_eq!(docs.body, b"docs index");

    let redirect = get(server.port, "/docs?x=1");
    assert_eq!(redirect.status, 308, "{}", redirect.headers);
    assert_eq!(
        redirect.header_value("Location").as_deref(),
        Some("/docs/?x=1")
    );

    let redirect = get(server.port, "/docs");
    assert_eq!(redirect.status, 308);
    assert_eq!(redirect.header_value("Location").as_deref(), Some("/docs/"));

    assert_eq!(get(server.port, "/emptydir/").status, 404);
    let no_index = get(server.port, "/emptydir");
    assert_eq!(no_index.status, 404);
    assert!(
        no_index.header_value("Location").is_none(),
        "directory without index.html must 404 rather than redirect to a dead URL"
    );

    let head_redirect = head(server.port, "/docs");
    assert_eq!(head_redirect.status, 308);
    assert_eq!(
        head_redirect.header_value("Location").as_deref(),
        Some("/docs/")
    );
    assert!(head_redirect.body.is_empty());

    assert_eq!(get(server.port, "/missing.txt").status, 404);
    assert_eq!(get(server.port, "/missing.js").status, 404);
    assert_eq!(get(server.port, "/file.txt/").status, 404);
}

#[test]
fn encoded_trailing_slash_is_a_directory_request() {
    let root = html_root(&[
        ("index.html", b"root"),
        ("docs/index.html", b"docs index"),
        ("file.txt", b"file"),
    ]);
    let server = TestServer::start(root.path());

    let docs = get(server.port, "/docs%2f");
    assert_eq!(docs.status, 200);
    assert_eq!(docs.body, b"docs index");

    let docs = get(server.port, "/docs%2F?x=1");
    assert_eq!(docs.status, 200);
    assert_eq!(docs.body, b"docs index");

    assert_eq!(get(server.port, "/file.txt%2f").status, 404);
    assert_eq!(get(server.port, "/missing%2f").status, 404);
    assert_eq!(get(server.port, "/%2f").status, 400);
}

#[test]
fn head_error_responses_have_headers_but_no_body() {
    let root = html_root(&[("index.html", b"ok"), ("docs/index.html", b"docs")]);
    let server = TestServer::start(root.path());
    let port = server.port;

    for (target, status) in [("/missing.txt", 404), ("/%2e%2e/x", 403), ("/.env", 404)] {
        let response = head(port, target);
        assert_eq!(response.status, status, "{target}");
        assert!(
            response.header_value("Content-Length").is_some(),
            "{target}"
        );
        assert_eq!(
            response.header_value("Content-Type").as_deref(),
            Some("text/plain"),
            "{target}"
        );
        assert!(response.body.is_empty(), "{target} must not send a body");
    }

    let dup_head = exchange(
        port,
        &format!("HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(dup_head.status, 400);
    assert!(
        dup_head.body.is_empty(),
        "HEAD error response must not send a body"
    );

    let dup_get = exchange(
        port,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(dup_get.status, 400);
    assert!(!dup_get.body.is_empty());

    let padded = "x".repeat(20 * 1024);
    let over_head = exchange(
        port,
        &format!("HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Pad: {padded}\r\n\r\n"),
    );
    assert_eq!(over_head.status, 431);
    assert!(over_head.body.is_empty());

    let over_get = exchange(
        port,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Pad: {padded}\r\n\r\n"),
    );
    assert_eq!(over_get.status, 431);
}

#[test]
fn post_is_rejected_without_writes() {
    let root = html_root(&[("index.html", b"original")]);
    let server = TestServer::start(root.path());

    let response = exchange(
        server.port,
        &format!(
            "POST /index.html HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 5\r\n\r\nhello",
            server.port
        ),
    );
    assert_eq!(response.status, 405, "{}", response.headers);
    assert_eq!(response.header_value("Allow").as_deref(), Some("GET, HEAD"));
    assert_eq!(
        fs::read(root.path().join("index.html")).unwrap(),
        b"original"
    );

    for method in ["PUT", "DELETE", "OPTIONS", "TRACE"] {
        let response = exchange(
            server.port,
            &format!(
                "{method} / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                server.port
            ),
        );
        assert_eq!(response.status, 405, "{method}");
    }
}

#[test]
fn traversal_and_encoded_attacks_denied() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("site");
    let sibling = parent.path().join("sibling");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&sibling).unwrap();
    fs::write(parent.path().join("secret.txt"), "SIBLING_SECRET_7f3a").unwrap();
    fs::write(sibling.join("inner.txt"), "SIBLING_INNER_5c1d").unwrap();
    fs::write(root.join("index.html"), b"ok").unwrap();
    fs::write(root.join(".env"), "IN_ROOT_SECRET_9b1c").unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git/config"), "GIT_SECRET").unwrap();

    let server = TestServer::start(&root);
    let cases = [
        "/../secret.txt",
        "/../sibling/inner.txt",
        "/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2e%2e/sibling/inner.txt",
        "/%2E%2E%2Fsecret.txt",
        "/..%2fsecret.txt",
        "/..%2fsibling%2finner.txt",
        "/a/../../secret.txt",
        "/%5cwindows%5csecret",
        "/a%5cb",
        "/%00index.html",
        "/index.html%00.txt",
        "/%ff%fe",
        "/%zz",
        "/%",
        "/%2",
        "//evil",
        "/%2fevil",
        "/%2f%2fevil",
        "/C:%5csecret.txt",
        "/C:/x",
        "/con.txt",
        "/COM1",
        "/lpt3.log",
        "/aux",
        "/.env",
        "/.git/config",
        "/%2eenv",
        "/.%65nv",
        "/trailing./x",
        "/trailing%20/x",
    ];
    for target in cases {
        let response = get(server.port, target);
        assert_ne!(response.status, 200, "{target} must not be served");
        assert!(
            [400, 403, 404].contains(&response.status),
            "{target} -> {}",
            response.status
        );
        assert!(
            !String::from_utf8_lossy(&response.body).contains("SIBLING_SECRET_7f3a"),
            "{target} leaked outside file"
        );
        assert!(
            !String::from_utf8_lossy(&response.body).contains("SIBLING_INNER_5c1d"),
            "{target} leaked sibling directory file"
        );
        assert!(
            !String::from_utf8_lossy(&response.body).contains("IN_ROOT_SECRET_9b1c"),
            "{target} leaked dotfile"
        );
        assert!(
            !String::from_utf8_lossy(&response.body).contains("GIT_SECRET"),
            "{target} leaked .git"
        );
    }
    assert_eq!(get(server.port, "/").status, 200);
}

#[cfg(unix)]
#[test]
fn symlinks_never_served() {
    use std::os::unix::fs::symlink;

    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "LINK_SECRET_4d2e").unwrap();
    let root = html_root(&[("index.html", b"ok"), ("real.txt", b"real")]);

    symlink(
        outside.path().join("secret.txt"),
        root.path().join("linked.txt"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("dirlink")).unwrap();
    symlink("real.txt", root.path().join("selflink.txt")).unwrap();

    let server = TestServer::start(root.path());
    for target in [
        "/linked.txt",
        "/dirlink/secret.txt",
        "/dirlink",
        "/dirlink/",
        "/selflink.txt",
    ] {
        let response = get(server.port, target);
        assert_eq!(response.status, 404, "{target}");
        assert!(
            !String::from_utf8_lossy(&response.body).contains("LINK_SECRET_4d2e"),
            "{target}"
        );
    }
    assert_eq!(get(server.port, "/real.txt").status, 200);
}

#[cfg(windows)]
#[test]
fn symlinks_never_served() {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "LINK_SECRET_4d2e").unwrap();
    let root = html_root(&[("index.html", b"ok"), ("real.txt", b"real")]);

    let file_link = symlink_file(
        outside.path().join("secret.txt"),
        root.path().join("linked.txt"),
    );
    let dir_link = symlink_dir(outside.path(), root.path().join("dirlink"));
    if file_link.is_err() || dir_link.is_err() {
        return;
    }
    let _ = symlink_file("real.txt", root.path().join("selflink.txt"));

    let server = TestServer::start(root.path());
    for target in [
        "/linked.txt",
        "/dirlink/secret.txt",
        "/dirlink/",
        "/selflink.txt",
    ] {
        let response = get(server.port, target);
        assert_eq!(response.status, 404, "{target}");
        assert!(
            !String::from_utf8_lossy(&response.body).contains("LINK_SECRET_4d2e"),
            "{target}"
        );
    }
}

#[cfg(unix)]
#[test]
fn fifo_never_blocks_or_serves() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let root = html_root(&[("index.html", b"ok")]);
    let fifo_path = CString::new(root.path().join("pipe").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o644) }, 0);

    let server = TestServer::start(root.path());
    let began = Instant::now();
    let response = get(server.port, "/pipe");
    assert_eq!(response.status, 404);
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "FIFO request must not hang"
    );
}

#[test]
fn host_header_is_validated() {
    let root = html_root(&[("index.html", b"ok")]);
    let server = TestServer::start(root.path());
    let port = server.port;

    assert_eq!(get(port, "/").status, 200);

    let localhost = exchange(
        port,
        &format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    );
    assert_eq!(localhost.status, 200);

    let unknown = exchange(port, "GET / HTTP/1.1\r\nHost: evil.com\r\n\r\n");
    assert_eq!(unknown.status, 403);

    let wrong_port = exchange(port, "GET / HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n");
    assert_eq!(wrong_port.status, 403);

    let missing = exchange(port, "GET / HTTP/1.1\r\n\r\n");
    assert_eq!(missing.status, 400);

    let duplicate = exchange(
        port,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(duplicate.status, 400);
}

#[test]
fn header_limits_and_malformed_requests() {
    let root = html_root(&[("index.html", b"ok")]);
    let server = TestServer::start(root.path());
    let port = server.port;

    let oversized = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Pad: {}\r\n\r\n",
        "x".repeat(20 * 1024)
    );
    assert_eq!(exchange(port, &oversized).status, 431);

    let mut many = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for idx in 0..80 {
        many.push_str(&format!("X-H{idx}: v\r\n"));
    }
    many.push_str("\r\n");
    assert_eq!(exchange(port, &many).status, 431);

    assert_eq!(exchange(port, "garbage\r\n\r\n").status, 400);
    assert_eq!(exchange(port, "GET / HTTP/9.9\r\n\r\n").status, 400);

    assert_eq!(get(port, "/").status, 200);
}

#[test]
fn idle_partial_request_does_not_block_others() {
    let root = html_root(&[("index.html", b"ok")]);
    let server = TestServer::start(root.path());

    let mut idle = connect(server.port);
    idle.write_all(b"GET /stuck HTTP/1.1\r\nHost: 12").unwrap();
    idle.flush().unwrap();

    let began = Instant::now();
    let response = get(server.port, "/");
    assert_eq!(response.status, 200);
    assert!(began.elapsed() < Duration::from_secs(4));
    drop(idle);
}

#[test]
fn stop_returns_quickly_with_idle_connection() {
    let root = html_root(&[("index.html", b"ok")]);
    let mut server = TestServer::start(root.path());

    let mut idle = connect(server.port);
    idle.write_all(b"GET /stuck HTTP/1.1\r\n").unwrap();
    idle.flush().unwrap();

    let began = Instant::now();
    server.stop_and_join(Duration::from_secs(3));
    assert!(began.elapsed() < Duration::from_secs(3));
    drop(idle);
}

#[test]
fn new_connections_refused_after_stop() {
    let root = html_root(&[("index.html", b"x")]);
    let mut server = TestServer::start(root.path());
    let port = server.port;
    server.stop_and_join(Duration::from_secs(3));

    let connect = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    );
    assert!(connect.is_err(), "connection accepted after server stop");
}

#[test]
fn incomplete_requests_time_out_with_408() {
    let root = html_root(&[("index.html", b"ok")]);
    let server = TestServer::start(root.path());

    let mut get_stream = connect(server.port);
    let mut head_stream = connect(server.port);
    for stream in [&mut get_stream, &mut head_stream] {
        stream
            .set_read_timeout(Some(Duration::from_secs(12)))
            .unwrap();
    }
    get_stream.write_all(b"GET / HTTP/1.1\r\nHost: 12").unwrap();
    head_stream
        .write_all(b"HEAD / HTTP/1.1\r\nHost: 12")
        .unwrap();

    let began = Instant::now();
    let mut raw_get = Vec::new();
    let mut raw_head = Vec::new();
    let _ = get_stream.read_to_end(&mut raw_get);
    let _ = head_stream.read_to_end(&mut raw_head);
    let elapsed = began.elapsed();

    let get_response = parse_response(&raw_get);
    assert_eq!(get_response.status, 408, "{raw_get:?}");
    assert!(!get_response.body.is_empty());

    let head_response = parse_response(&raw_head);
    assert_eq!(head_response.status, 408, "{raw_head:?}");
    assert!(
        head_response.body.is_empty(),
        "HEAD timeout response must not send a body"
    );

    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(12),
        "request deadline elapsed in {elapsed:?}"
    );
    assert_eq!(get(server.port, "/").status, 200);
}

#[test]
fn stop_interrupts_in_flight_stream() {
    let payload = vec![7u8; 8 * 1024 * 1024];
    let root = html_root(&[("index.html", b"ok")]);
    fs::write(root.path().join("big.bin"), &payload).unwrap();
    let mut server = TestServer::start(root.path());

    let mut stream = connect(server.port);
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(
            format!(
                "GET /big.bin HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                server.port
            )
            .as_bytes(),
        )
        .unwrap();
    let mut chunk = [0u8; 8192];
    let _ = stream.read(&mut chunk);

    let began = Instant::now();
    server.stop_and_join(Duration::from_secs(3));
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "in-flight stream blocked shutdown"
    );
}

#[cfg(unix)]
#[test]
fn readonly_root_serves_without_writes() {
    use std::os::unix::fs::PermissionsExt;

    let root = html_root(&[("index.html", b"frozen"), ("sub/a.txt", b"nested")]);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let server = TestServer::start(root.path());
    let response = get(server.port, "/");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"frozen");
    assert_eq!(get(server.port, "/sub/a.txt").body, b"nested");

    let post = exchange(
        server.port,
        &format!(
            "POST /index.html HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 4\r\n\r\njunk",
            server.port
        ),
    );
    assert_eq!(post.status, 405);
    assert_eq!(fs::read(root.path().join("index.html")).unwrap(), b"frozen");

    drop(server);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
}

fn cli_base(root: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_srvm"));
    cmd.env("PATH", "")
        .env_remove("PORT")
        .env_remove("PORT_FIXTURE_MODE")
        .env_remove("PORT_FIXTURE_LOG")
        .env_remove("BROWSER")
        .env("NO_COLOR", "1")
        .arg("--no-color")
        .arg(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn cli(root: &TempDir) -> Command {
    let mut cmd = cli_base(root);
    cmd.arg("--no-open");
    cmd
}

fn line_reader(stream: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn wait_line(rx: &mpsc::Receiver<String>, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) if line.contains(needle) => return line,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for output containing {needle:?}"),
        }
    }
}

fn drain(rx: &mpsc::Receiver<String>, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut collected = String::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                collected.push_str(&line);
                collected.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    collected
}

fn app_line_port(line: &str) -> u16 {
    line.split("http://127.0.0.1:")
        .nth(1)
        .unwrap()
        .trim_end_matches(|ch: char| !ch.is_ascii_digit())
        .parse()
        .unwrap()
}

fn held_adjacent_pair() -> (TcpListener, u16, u16) {
    for _ in 0..128 {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        if port == u16::MAX {
            drop(held);
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return (held, port, port + 1);
        }
    }
    panic!("could not find adjacent free ports");
}

#[test]
fn cli_serves_index_html_on_os_port() {
    let root = html_root(&[("index.html", b"<h1>hello</h1>\n")]);

    let mut cmd = cli(&root);
    cmd.arg("--port").arg("0");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    wait_line(&out, "built-in static server", Duration::from_secs(10));
    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(10),
    );
    let port = app_line_port(&app_line);
    assert_ne!(port, 0);

    let rest = drain(&out, Duration::from_millis(500));
    assert!(!rest.contains("installing"), "{rest}");

    let response = get(port, "/");
    assert_eq!(response.status, 200, "{}", response.headers);
    assert_eq!(response.body, b"<h1>hello</h1>\n");
}

#[test]
fn cli_walks_forward_from_busy_start_port() {
    let root = html_root(&[("index.html", b"busy-start")]);
    let (holder, start, _next) = held_adjacent_pair();

    let mut cmd = cli(&root);
    cmd.arg("--port").arg(start.to_string());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    wait_line(&out, &format!("{start} busy ->"), Duration::from_secs(10));
    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(10),
    );
    let port = app_line_port(&app_line);
    assert!(port > start, "walked port {port} must be above {start}");

    let response = get(port, "/");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"busy-start");

    let probe = connect(start);
    drop(probe);
    drop(holder);
}

#[test]
fn cli_dry_run_never_binds_or_spawns() {
    let root = html_root(&[("index.html", b"dry")]);
    let (holder, start, _next) = held_adjacent_pair();

    let output = cli(&root)
        .arg("--dry-run")
        .arg("--port")
        .arg(start.to_string())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("command    built-in static server"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("port       {start} (start")),
        "{stdout}"
    );
    assert!(
        stdout.contains("built-in loopback listener (no child process)"),
        "{stdout}"
    );
    assert!(!stdout.contains("unsupported"), "{stdout}");
    assert!(!stdout.contains("install"), "{stdout}");

    let probe = connect(start);
    drop(probe);
    drop(holder);
}

#[test]
fn cli_dry_run_defaults_to_8000_start() {
    let root = html_root(&[("index.html", b"dry")]);
    let output = cli(&root).arg("--dry-run").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("port       8000 (start"), "{stdout}");
}

#[test]
fn cli_default_launch_starts_at_8000() {
    let root = html_root(&[("index.html", b"default")]);
    let mut cmd = cli(&root);
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(15),
    );
    let port = app_line_port(&app_line);
    assert!(port >= 8000, "default walk must start at 8000, got {port}");

    let response = get(port, "/");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"default");
}

#[cfg(unix)]
fn fake_browser() -> (TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let script = dir.path().join("fake-browser");
    fs::write(&script, "#!/bin/sh\n: > \"$SRVM_MARKER\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, script)
}

#[cfg(unix)]
#[test]
fn browser_runs_when_open_is_allowed() {
    let root = html_root(&[("index.html", b"ok")]);
    let (marker_dir, script) = fake_browser();
    let marker = marker_dir.path().join("browser-opened");

    let mut cmd = cli_base(&root);
    cmd.env("BROWSER", &script)
        .env("SRVM_MARKER", &marker)
        .arg("--port")
        .arg("0");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    wait_line(&out, "ctrl-c to stop", Duration::from_secs(10));

    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "allowed open never ran the configured browser"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn no_open_never_invokes_browser() {
    let root = html_root(&[("index.html", b"ok")]);
    let (marker_dir, script) = fake_browser();
    let marker = marker_dir.path().join("browser-opened");

    let mut cmd = cli(&root);
    cmd.env("BROWSER", &script)
        .env("SRVM_MARKER", &marker)
        .arg("--port")
        .arg("0");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(10),
    );
    let port = app_line_port(&app_line);
    assert_eq!(get(port, "/").status, 200);
    wait_line(&out, "ctrl-c to stop", Duration::from_secs(10));
    thread::sleep(Duration::from_secs(1));
    assert!(
        !marker.exists(),
        "--no-open must not launch the configured browser"
    );
}

#[cfg(unix)]
#[test]
fn sigint_exits_130_and_closes_socket() {
    cli_signal_exits_130(libc::SIGINT);
}

#[cfg(unix)]
#[test]
fn sigterm_exits_130_and_closes_socket() {
    cli_signal_exits_130(libc::SIGTERM);
}

#[cfg(unix)]
fn cli_signal_exits_130(signal: libc::c_int) {
    let root = html_root(&[("index.html", b"sig")]);
    let mut cmd = cli(&root);
    cmd.arg("--port").arg("0");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(10),
    );
    let port = app_line_port(&app_line);
    assert_eq!(get(port, "/").status, 200);

    unsafe {
        libc::kill(child.0.id() as libc::pid_t, signal);
    }
    let status = wait_exit(&mut child.0, Duration::from_secs(5));
    assert_eq!(status.code(), Some(130), "signal {signal} exit");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => return,
            Err(_) => {
                assert!(Instant::now() < deadline, "socket still bound after exit");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(unix)]
fn wait_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("child did not exit within {timeout:?}");
}

struct ChildGuard(Child);

impl ChildGuard {
    fn new(cmd: &mut Command) -> Self {
        Self(cmd.spawn().unwrap())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if matches!(self.0.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(self.0.id() as libc::pid_t, libc::SIGINT);
            }
            self.wait_bounded(Duration::from_secs(2));
        }

        #[cfg(windows)]
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &self.0.id().to_string()])
                .status();
            self.wait_bounded(Duration::from_secs(2));
        }

        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl ChildGuard {
    fn wait_bounded(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.0.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}
