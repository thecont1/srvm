use std::{
    env, fs,
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if let Some(log) = env::var_os("PORT_FIXTURE_LOG") {
        let record = format!(
            "PORT={}\nARGS={}\nSRVM_TEST_ENV={}\n",
            env::var("PORT").unwrap_or_default(),
            args.join(" "),
            env::var("SRVM_TEST_ENV").unwrap_or_default()
        );
        let _ = fs::write(log, record);
    }

    if args.first().map(String::as_str) == Some("install") {
        let marker = format!(
            "PORT={}\nARGS={}\n",
            env::var("PORT").unwrap_or_default(),
            args.join(" ")
        );
        let _ = fs::write("srvm-fixture-install.txt", marker);
        return;
    }

    let mode = env::var("PORT_FIXTURE_MODE").unwrap_or_else(|_| "env-port".into());

    // The Windows teardown regression runs this mode: the app and a spawned
    // descendant both swallow console events themselves, so their death proves
    // srvm's tree teardown rather than the console default handler.
    if mode == "hold-break" {
        #[cfg(windows)]
        windows_console::resist_console_break();
        if let Some(pid_file) = env::var_os("PORT_FIXTURE_PID_FILE") {
            let _ = fs::write(&pid_file, format!("{}\n", std::process::id()));
        }
        if let Some(child_pid_file) = env::var_os("PORT_FIXTURE_CHILD_PID_FILE") {
            let mut child = std::process::Command::new(env::current_exe().expect("fixture exe path"));
            child
                .arg("--child")
                .env("PORT_FIXTURE_MODE", "hold-break-child")
                .env("PORT_FIXTURE_CHILD_PID_FILE", &child_pid_file);
            let _ = child.spawn().expect("spawn the break-resistant descendant");
        }
        // Fall through: serve as a normal app so srvm announces a real URL.
    }

    if mode == "hold-break-child" {
        #[cfg(windows)]
        windows_console::resist_console_break();
        if let Some(pid_file) = env::var_os("PORT_FIXTURE_CHILD_PID_FILE") {
            let _ = fs::write(&pid_file, format!("{}\n", std::process::id()));
        }
        std::thread::sleep(Duration::from_secs(300));
        return;
    }

    // The snipe tests need a child that is slow to bind. `sniped-argv-port` and
    // `fail-after-hold` always wait; any other mode waits only when a test asks
    // for it, so a test can steal the port srvm reserved for that child.
    let hold = matches!(mode.as_str(), "sniped-argv-port" | "fail-after-hold")
        || env::var_os("PORT_FIXTURE_HOLD").is_some();
    if hold {
        println!("fixture waiting before bind");
        let release = env::var_os("PORT_FIXTURE_RELEASE").expect("missing fixture release path");
        let deadline = Instant::now() + Duration::from_secs(15);
        while fs::metadata(&release).is_err() {
            assert!(Instant::now() < deadline, "timed out waiting to bind");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    if mode == "fail-after-hold" {
        eprintln!("boom");
        std::process::exit(7);
    }

    // Dies without announcing a URL, but slowly: a handoff loss detected after
    // srvm's quick-death window, which must still be retried. The second run
    // behaves normally, so a retry is observable as a serving app.
    if mode == "fail-once-slow" {
        let marker = env::var_os("PORT_FIXTURE_ONCE").expect("missing once marker path");
        if fs::metadata(&marker).is_err() {
            fs::write(&marker, "1").unwrap();
            std::thread::sleep(Duration::from_secs(4));
            eprintln!("boom after a slow start");
            std::process::exit(7);
        }
    }

    let port = match mode.as_str() {
        "ignore-port" => 0,
        "argv-port" | "sniped-argv-port" => argv_port(&args).unwrap_or(0),
        "fail-once-slow" => env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        _ => env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    };

    let excluded = if mode == "ignore-port" {
        env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .and_then(|port| TcpListener::bind(("127.0.0.1", port)).ok())
    } else {
        None
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("fixture bind failed");
    drop(excluded);
    let actual = listener.local_addr().unwrap().port();
    if mode != "silent-http" {
        println!("ready on http://127.0.0.1:{actual}/");
    }

    listener.set_nonblocking(true).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Handle each connection on its own thread: stray connections
                // from parallel tests can block for seconds on read, and a
                // sequential loop would starve the real request behind them.
                let done = std::sync::Arc::clone(&done);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 1024];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let mut parts = request.split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let path = parts.next().unwrap_or("/");
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                    // Parallel tests reuse OS-assigned ports, so a stray GET
                    // can land here. Only exit for a GET that was addressed
                    // to this fixture (the Host header carries the port) or
                    // an explicit shutdown path; everything else is served
                    // and ignored.
                    let host_port = request
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("host").then(|| value.trim())
                        })
                        .and_then(|host| host.rsplit(':').next())
                        .and_then(|port| port.parse::<u16>().ok());
                    let stop = (method == "GET" && host_port == Some(actual)) || path == "/shutdown";
                    // Close the stream gracefully first so the response is
                    // delivered with FIN (Windows resets sockets that are
                    // still open when the process dies), then signal main.
                    drop(stream);
                    if stop {
                        done.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
            // Transient accept errors (ECONNABORTED under load) must not
            // kill the server; keep serving until the deadline or a GET.
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
        if done.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
    }
}

fn argv_port(args: &[String]) -> Option<u16> {
    for (i, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--port=") {
            return value.parse().ok();
        }
        if matches!(arg.as_str(), "--port" | "-p" | "-P" | "-a") {
            let value = args.get(i + 1)?;
            if let Some((_, port)) = value.rsplit_once(':') {
                return port.parse().ok();
            }
            return value.parse().ok();
        }
    }
    args.iter().rev().find_map(|arg| arg.parse::<u16>().ok())
}

/// Console-control support for the Windows teardown regression.
#[cfg(windows)]
mod windows_console {
    type HandlerRoutine = unsafe extern "system" fn(u32) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
    }

    /// Install a handler that swallows console control events. The regression
    /// relies on this: an app or descendant that dies anyway cannot have been
    /// killed by the event itself, so its death is srvm's teardown doing work.
    pub fn resist_console_break() {
        unsafe extern "system" fn ignore(_event: u32) -> i32 {
            1 // TRUE: handled; do not pass it on or terminate.
        }
        unsafe {
            SetConsoleCtrlHandler(Some(ignore), 1);
        }
    }
}
