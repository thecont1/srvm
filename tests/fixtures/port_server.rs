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
            "PORT={}\nARGS={}\n",
            env::var("PORT").unwrap_or_default(),
            args.join(" ")
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
    if matches!(mode.as_str(), "sniped-argv-port" | "fail-after-hold") {
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

    let port = match mode.as_str() {
        "ignore-port" => 0,
        "argv-port" | "sniped-argv-port" => argv_port(&args).unwrap_or(0),
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
