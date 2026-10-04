use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    detect::{CommandSpec, PortInjection, ServeSpec},
    ports, staticsrv,
};

mod collapse;
mod kill;
mod open;
mod pump;
pub mod ring;
pub mod scan;

use ring::Ring;

static CURRENT_CHILD: OnceLock<Arc<Mutex<Option<u32>>>> = OnceLock::new();
static STATIC_STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub struct SupervisorOptions {
    pub no_open: bool,
    pub no_install: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub no_color: bool,
    pub port: Option<u16>,
}

pub fn run(root: &Path, spec: &ServeSpec, options: SupervisorOptions) -> Result<()> {
    let stop = spec.is_static.then(|| {
        STATIC_STOP
            .get_or_init(|| Arc::new(AtomicBool::new(false)))
            .clone()
    });

    install_signal_handler()?;

    let requested = plan_port(spec, options)?;

    if let Some(stop) = stop {
        return run_static(root, requested, options, stop);
    }

    if !options.no_install
        && let Some(install) = &spec.install
    {
        println!(
            "  step       installing dependencies — {}",
            install.command_line()
        );
        run_install(root, install, options)?;
    }

    run_server(root, spec, requested, options)
}

fn run_static(
    root: &Path,
    requested: Option<u16>,
    options: SupervisorOptions,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let start = requested.unwrap_or(8000);
    let server = staticsrv::StaticServer::bind(root, start)?;
    let actual = server.local_addr()?.port();
    report_port(start, actual, options);
    if !options.quiet {
        println!("  step       starting — built-in static server");
    }

    let url = format!("http://{}/", server.local_addr()?);
    let mut announced = None;
    let mut opened = false;
    announce_reported_url(&url, Some(actual), &mut announced, &mut opened, options);

    server.serve(stop.clone())?;
    if stop.load(Ordering::SeqCst) {
        std::process::exit(130);
    }
    Ok(())
}

fn plan_port(spec: &ServeSpec, options: SupervisorOptions) -> Result<Option<u16>> {
    let inherited = match &spec.port {
        PortInjection::Env(key) => std::env::var(key).ok(),
        _ => None,
    };
    let requested = ports::requested_port(spec, options.port, inherited.as_deref())?;

    if matches!(spec.port, PortInjection::None)
        && (options.port.is_some() || spec.url_hint.is_some())
    {
        let mut warning = format!(
            "port overrides are unsupported for {}; leaving its ports unchanged",
            spec.name
        );
        if let Some(port) = options.port {
            warning.push_str(&format!("; --port {port} ignored"));
        }
        eprintln!("  warning    {warning}");
    }

    Ok(requested)
}

fn run_install(root: &Path, command: &CommandSpec, options: SupervisorOptions) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));
    let mut child = spawn(command, root, &[])?;
    set_current_child(Some(child.id()));
    let (tx, _rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), false, tx, options)?;
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            set_current_child(None);
            join_pumps(&mut joins);
            if status.success() {
                return Ok(());
            }
            return Err(error_with_tail(
                format!("install command failed: {}", command.command_line()),
                &ring,
            ));
        }

        if started.elapsed() > Duration::from_secs(15 * 60) {
            kill::terminate_tree(&mut child);
            set_current_child(None);
            join_pumps(&mut joins);
            return Err(error_with_tail(
                format!("install command timed out: {}", command.command_line()),
                &ring,
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn run_server(
    root: &Path,
    spec: &ServeSpec,
    requested: Option<u16>,
    options: SupervisorOptions,
) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));

    let mut reservation = None;
    let (command, env, selected) = match requested {
        Some(start) => {
            let listener = ports::reserve(start)
                .with_context(|| format!("could not find a free port starting at {start}"))?;
            let selected = listener.local_addr()?.port();
            reservation = Some(listener);
            let (command, env) = ports::apply(spec, selected);
            (command, env, Some(selected))
        }
        None => (spec.command.clone(), Vec::new(), None),
    };

    if let (Some(start), Some(selected)) = (requested, selected) {
        report_port(start, selected, options);
    }

    println!("  step       starting — {}", command.command_line());
    drop(reservation);
    let mut child = spawn(&command, root, &env)?;
    set_current_child(Some(child.id()));

    let (tx, rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), true, tx, options)?;
    let started = Instant::now();
    let probe_port = selected.or(spec.url_hint);
    let mut announced: Option<String> = None;
    let mut opened = false;
    let mut probed_hint = false;

    loop {
        while let Ok(url) = rx.try_recv() {
            announce_reported_url(&url, selected, &mut announced, &mut opened, options);
        }

        if announced.is_none()
            && !probed_hint
            && started.elapsed() >= Duration::from_secs(12)
            && let Some(port) = probe_port
        {
            probed_hint = true;
            if let Some(url) = probe_hint(port) {
                announce_reported_url(&url, selected, &mut announced, &mut opened, options);
            }
        }

        if let Some(status) = child.try_wait()? {
            set_current_child(None);
            join_pumps(&mut joins);
            while let Ok(url) = rx.try_recv() {
                announce_reported_url(&url, selected, &mut announced, &mut opened, options);
            }
            if status.success() {
                if announced.is_some() {
                    println!("  exited     {}", status);
                }
                return Ok(());
            }

            if started.elapsed() < Duration::from_secs(3) {
                return Err(error_with_tail(
                    format!("{} failed early: {status}", spec.name),
                    &ring,
                ));
            }

            return Err(error_with_tail(
                format!("{} exited: {status}", spec.name),
                &ring,
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn attach_pumps(
    child: &mut std::process::Child,
    ring: Arc<Mutex<Ring>>,
    detect_urls: bool,
    tx: mpsc::Sender<String>,
    options: SupervisorOptions,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let url = Arc::new(Mutex::new(None));
    let mut joins = Vec::new();

    if let Some(stdout) = child.stdout.take() {
        joins.push(pump::spawn_pump(
            stdout,
            ring.clone(),
            url.clone(),
            tx.clone(),
            pump::PumpOptions {
                verbose: options.verbose,
                quiet: options.quiet,
                detect_urls,
                no_color: options.no_color,
            },
        ));
    }

    if let Some(stderr) = child.stderr.take() {
        joins.push(pump::spawn_pump(
            stderr,
            ring,
            url,
            tx,
            pump::PumpOptions {
                verbose: options.verbose,
                quiet: options.quiet,
                detect_urls,
                no_color: options.no_color,
            },
        ));
    }

    Ok(joins)
}

fn spawn(
    command: &CommandSpec,
    root: &Path,
    env: &[(String, String)],
) -> Result<std::process::Child> {
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .current_dir(root)
        .env("BROWSER", "none")
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    kill::configure_process_group(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("failed to spawn {}", command.command_line()))
}

fn report_port(requested: u16, selected: u16, options: SupervisorOptions) {
    if options.quiet {
        return;
    }
    if requested == 0 {
        println!("  port       selected {selected}");
    } else if requested != selected {
        println!("  port       {requested} busy -> {selected}");
    } else {
        println!("  port       {selected}");
    }
}

fn announce_reported_url(
    url: &str,
    selected: Option<u16>,
    announced: &mut Option<String>,
    opened: &mut bool,
    options: SupervisorOptions,
) {
    if announced.as_deref() == Some(url) {
        return;
    }
    *announced = Some(url.to_string());

    println!("  app        {url}");
    if !*opened {
        *opened = true;
        if !options.no_open {
            let _ = open::open_browser(url);
        }
        println!("  ctrl-c to stop");
    }
    if let Some(expected) = selected
        && let Some(actual) = ports::url_port(url)
        && actual != expected
    {
        eprintln!("  port       requested {expected}, app reports {url}; override ignored");
    }
}

fn probe_hint(port: u16) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .write_all(
            format!("HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(250);
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    let end = loop {
        if let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
            break end;
        }
        if buf.len() == 256 {
            return None;
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).ok()?;
        let limit = 256 - buf.len();
        let n = stream.read(&mut chunk[..limit]).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let mut status = std::str::from_utf8(&buf[..end]).ok()?.split_whitespace();
    match status.next()? {
        "HTTP/1.0" | "HTTP/1.1" => {}
        _ => return None,
    }
    let raw_code = status.next()?;
    if raw_code.len() != 3 || !raw_code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let code: u16 = raw_code.parse().ok()?;
    (100..=599)
        .contains(&code)
        .then(|| format!("http://127.0.0.1:{port}"))
}

fn error_with_tail(message: String, ring: &Arc<Mutex<Ring>>) -> anyhow::Error {
    eprintln!("  failed     {message}");
    for line in ring.lock().expect("ring lock poisoned").tail(12) {
        eprintln!("    {line}");
    }
    anyhow::anyhow!(message)
}

fn install_signal_handler() -> Result<()> {
    let child = CURRENT_CHILD
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone();

    ctrlc::set_handler(move || {
        if let Some(stop) = STATIC_STOP.get()
            && !stop.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let pid = *child.lock().expect("signal child lock poisoned");
        if let Some(pid) = pid {
            kill::terminate_tree_by_pid(pid);
        }
        std::process::exit(130);
    })?;
    Ok(())
}

fn set_current_child(pid: Option<u32>) {
    if let Some(slot) = CURRENT_CHILD.get() {
        *slot.lock().expect("signal child lock poisoned") = pid;
    }
}

fn join_pumps(joins: &mut Vec<thread::JoinHandle<()>>) {
    for join in joins.drain(..) {
        let _ = join.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{SupervisorOptions, announce_reported_url, probe_hint};
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
        time::{Duration, Instant},
    };

    fn probe_server(
        respond: impl FnOnce(&mut TcpStream) + Send + 'static,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                respond(&mut stream);
            }
        });
        (port, handle)
    }

    fn http_response(response: &'static [u8]) -> impl FnOnce(&mut TcpStream) + Send + 'static {
        move |stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
        }
    }

    #[test]
    fn probe_hint_adopts_port_serving_http() {
        let (port, handle) = probe_server(http_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ));

        assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_accepts_fragmented_status_line() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            stream.write_all(b"HTTP/1.1 2").unwrap();
            thread::sleep(Duration::from_millis(30));
            stream.write_all(b"00 OK\r\n").unwrap();
        });

        assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_counts_http_errors_as_alive() {
        for status in [
            b"HTTP/1.1 404 Not Found\r\n\r\n" as &[u8],
            b"HTTP/1.0 500 Server Error\r\n\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_tcp_only_endpoint() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_stalled_endpoint() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            thread::sleep(Duration::from_millis(400));
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_slow_drip_respects_total_deadline() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            for byte in b"HTTP/1.1 200 OK\r\n" {
                if stream.write_all(&[*byte]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(80));
            }
        });

        let started = Instant::now();
        assert_eq!(probe_hint(port), None);
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "probe should give up at the 250ms total deadline"
        );
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_missing_newline() {
        let (port, handle) = probe_server(http_response(b"HTTP/1.1 200 OK"));

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_malformed_status_codes() {
        for status in [
            b"HTTP/1.1 0200\r\n" as &[u8],
            b"HTTP/1.1 +200\r\n",
            b"HTTP/1.1 20\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), None);
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_overlong_status_line() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(&[b'X'; 300]);
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_non_http_and_out_of_range() {
        for status in [
            b"garbage\r\n" as &[u8],
            b"HTTP/2 200\r\n\r\n",
            b"HTTP/1.1 999 Weird\r\n\r\n",
            b"HTTP/1.1 099 Nope\r\n\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), None);
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_closed_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert_eq!(probe_hint(port), None);
    }

    fn quiet_options() -> SupervisorOptions {
        SupervisorOptions {
            no_open: true,
            no_install: false,
            verbose: false,
            quiet: true,
            no_color: true,
            port: None,
        }
    }

    #[test]
    fn first_announcement_opens_browser_once() {
        let mut announced = None;
        let mut opened = false;
        let options = quiet_options();

        announce_reported_url(
            "http://127.0.0.1:8000",
            None,
            &mut announced,
            &mut opened,
            options,
        );
        assert_eq!(announced.as_deref(), Some("http://127.0.0.1:8000"));
        assert!(opened);

        announce_reported_url(
            "http://127.0.0.1:8000",
            None,
            &mut announced,
            &mut opened,
            options,
        );
        assert_eq!(announced.as_deref(), Some("http://127.0.0.1:8000"));
        assert!(opened);
    }

    #[test]
    fn late_sniffed_url_replaces_probe_announcement() {
        let mut announced = Some("http://127.0.0.1:8123".to_string());
        let mut opened = true;
        let options = quiet_options();

        announce_reported_url(
            "http://127.0.0.1:9123/app?x=1#f",
            Some(8123),
            &mut announced,
            &mut opened,
            options,
        );
        assert_eq!(
            announced.as_deref(),
            Some("http://127.0.0.1:9123/app?x=1#f")
        );
        assert!(opened);

        announce_reported_url(
            "http://127.0.0.1:9123/app?x=1#f",
            Some(8123),
            &mut announced,
            &mut opened,
            options,
        );
        assert_eq!(
            announced.as_deref(),
            Some("http://127.0.0.1:9123/app?x=1#f")
        );
        assert!(opened);
    }
}
