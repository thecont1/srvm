use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use tempfile::{TempDir, tempdir};

#[test]
fn occupied_start_shifts_env_port_and_install_sees_no_port() {
    let (held, busy, _first_free) = occupied_with_free_next();
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "env-port", &log);
    cmd.arg("--port").arg(busy.to_string()).arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    // A parallel test may claim the successor port before srvm reserves or
    // binds it; srvm walks or retries internally, so the authoritative port
    // is the last "busy ->" line before the announcement.
    let (seen, app_line) = wait_announcement(&out, Duration::from_secs(25));
    let announced = app_line_port(&app_line);
    let selected: u16 = seen
        .iter()
        .rev()
        .find_map(|line| {
            line.trim_start()
                .strip_prefix(&format!("port       {busy} busy -> "))
                .and_then(|rest| rest.trim().parse().ok())
        })
        .expect("srvm never reported a shifted port");
    assert_eq!(announced, selected);
    assert_ne!(selected, busy);

    let response = http_get(selected);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());

    assert!(TcpListener::bind(("127.0.0.1", busy)).is_err());
    drop(held);

    let marker = fs::read_to_string(repo.path().join("srvm-fixture-install.txt")).unwrap();
    assert!(marker.contains("PORT=\n"), "install saw a port: {marker}");
    let log = fs::read_to_string(&log).unwrap();
    assert!(
        log.contains(&format!("PORT={selected}")),
        "server env missing shifted port: {log}"
    );
    assert!(log.contains("ARGS=run dev"), "{log}");
}

#[test]
fn port_zero_injects_os_assigned_port_via_args() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "wrangler");
    fs::write(repo.path().join("wrangler.toml"), "name = 'worker'\n").unwrap();
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "argv-port", &log);
    cmd.arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let (seen, app_line) = wait_announcement(&out, Duration::from_secs(20));
    let selected: u16 = seen
        .iter()
        .rev()
        .find_map(|line| {
            line.trim_start()
                .strip_prefix("port       selected ")
                .and_then(|rest| rest.trim().parse().ok())
        })
        .expect("srvm never reported a selected port");
    assert_ne!(selected, 0);
    assert_eq!(app_line_port(&app_line), selected);

    let log = fs::read_to_string(&log).unwrap();
    assert!(
        log.contains(&format!("ARGS=dev --port {selected}")),
        "wrangler argv missing injected port: {log}"
    );

    assert!(http_get(selected).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());
}

#[test]
fn app_ignoring_injection_keeps_sniffed_url() {
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "ignore-port", &log);
    cmd.arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let port_line = wait_line(&out, "port       selected ", Duration::from_secs(15));
    let selected: u16 = port_line
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap();

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(15),
    );
    let actual = app_line_port(&app_line);
    assert_ne!(actual, selected);

    let warning = wait_line(
        &err,
        &format!("requested {selected}, app reports"),
        Duration::from_secs(10),
    );
    assert!(warning.contains("override ignored"), "{warning}");

    assert!(http_get(actual).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());
}

#[test]
fn dry_run_never_allocates_or_launches() {
    let (held, busy, _) = occupied_with_free_next();
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let output = srvm(bin.path(), "env-port", &log)
        .arg("--dry-run")
        .arg("--port")
        .arg(busy.to_string())
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("command    npm run dev"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "port       {busy} (start; availability checked at launch)"
        )),
        "{stdout}"
    );
    assert!(stdout.contains("PORT"), "{stdout}");

    assert!(!repo.path().join("srvm-fixture-install.txt").exists());
    assert!(!log.exists());
    assert!(TcpListener::bind(("127.0.0.1", busy)).is_err());
    drop(held);
}

#[test]
fn no_install_still_launches_without_marker() {
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "env-port", &log);
    cmd.arg("--no-install").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(15),
    );
    assert!(http_get(app_line_port(&app_line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());

    assert!(!repo.path().join("srvm-fixture-install.txt").exists());
}

#[test]
fn compose_warns_and_leaves_ports_unchanged() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "docker");
    fs::write(repo.path().join("compose.yaml"), "services: {}\n").unwrap();
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "env-port", &log);
    cmd.arg("--port").arg("8080").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    wait_line(
        &err,
        "port overrides are unsupported for compose",
        Duration::from_secs(15),
    );

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(15),
    );
    assert!(http_get(app_line_port(&app_line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());

    let log = fs::read_to_string(&log).unwrap();
    assert!(log.contains("ARGS=compose up"), "{log}");
    assert!(log.contains("PORT=\n"), "{log}");
}

#[test]
fn opaque_script_without_request_stays_untouched() {
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "env-port", &log);
    cmd.arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_line(
        &out,
        "app        http://127.0.0.1:",
        Duration::from_secs(15),
    );
    assert!(http_get(app_line_port(&app_line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());

    let log = fs::read_to_string(&log).unwrap();
    assert!(log.contains("PORT=\n"), "unexpected injected port: {log}");
    assert!(log.contains("ARGS=run dev"), "{log}");
}

#[test]
fn dropping_guard_reaps_server_tree() {
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let actual;
    {
        let mut cmd = srvm(bin.path(), "env-port", &log);
        cmd.arg(repo.path());
        let mut child = ChildGuard::new(&mut cmd);
        let out = line_reader(child.0.stdout.take().unwrap());
        let _err = line_reader(child.0.stderr.take().unwrap());

        let app_line = wait_line(
            &out,
            "app        http://127.0.0.1:",
            Duration::from_secs(15),
        );
        actual = app_line_port(&app_line);
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpListener::bind(("127.0.0.1", actual)) {
            Ok(listener) => {
                drop(listener);
                break;
            }
            Err(err) if Instant::now() < deadline => {
                let _ = err;
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => panic!("fixture port still held after guard drop: {err}"),
        }
    }
}

#[test]
fn cli_rejects_out_of_range_port() {
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    let log = repo.path().join("fixture.log");

    let output = srvm(bin.path(), "env-port", &log)
        .arg("--port")
        .arg("65536")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("65536"), "{stderr}");
}

#[test]
fn silent_server_is_found_by_probing_the_shifted_port() {
    let (mut decoy, decoy_port, _first_free) = decoy_with_free_next();
    let repo = package_repo(r#"{"scripts":{"dev":"node server.js"}}"#);
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    let log = repo.path().join("fixture.log");

    let mut cmd = srvm(bin.path(), "silent-http", &log);
    cmd.arg("--port")
        .arg(decoy_port.to_string())
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());

    // srvm may retry internally if the reserved port is sniped mid-handoff;
    // the announced port just has to be a real shifted port, not the decoy's.
    let (_seen, app_line) = wait_announcement(&out, Duration::from_secs(30));
    let announced = app_line_port(&app_line);
    assert_ne!(
        announced, decoy_port,
        "srvm must announce the shifted port, not the busy one"
    );

    assert!(http_get(announced).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(15)).success());

    let log = fs::read_to_string(&log).unwrap();
    assert!(
        log.contains(&format!("PORT={announced}")),
        "server env missing announced port: {log}"
    );

    let _ = http_get(decoy_port);
    let _ = decoy.0.wait();
}

fn package_repo(pkg: &str) -> TempDir {
    let repo = tempdir().unwrap();
    fs::write(repo.path().join("package.json"), pkg).unwrap();
    repo
}

fn srvm(bin: &Path, mode: &str, log: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_srvm"));
    cmd.env("PATH", bin)
        .env_remove("PORT")
        .env("PORT_FIXTURE_MODE", mode)
        .env("PORT_FIXTURE_LOG", log)
        .env("NO_COLOR", "1")
        .arg("--no-open")
        .arg("--no-color")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn fixture_bin() -> PathBuf {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    static BIN: OnceLock<PathBuf> = OnceLock::new();

    BIN.get_or_init(|| {
        let dir = DIR.get_or_init(|| tempdir().unwrap());
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/port_server.rs");
        let out = dir.path().join(if cfg!(windows) {
            "port_server.exe"
        } else {
            "port_server"
        });
        let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let status = Command::new(rustc)
            .arg("-O")
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .status()
            .expect("failed to invoke rustc for the port fixture");
        assert!(status.success(), "rustc failed compiling port fixture");
        out
    })
    .clone()
}

fn stub_exec(dir: &Path, name: &str) {
    let fixture = fixture_bin();

    #[cfg(windows)]
    {
        fs::write(
            dir.join(format!("{name}.cmd")),
            format!(
                "@echo off\r\n\"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
                fixture.display()
            ),
        )
        .unwrap();
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", fixture.display()),
        )
        .unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }
}

fn occupied_with_free_next() -> (TcpListener, u16, u16) {
    for _ in 0..25 {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        if port == u16::MAX {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return (held, port, port + 1);
        }
    }
    panic!("could not find an occupied port with a free successor");
}

fn decoy_with_free_next() -> (ChildGuard, u16, u16) {
    for _ in 0..25 {
        let mut cmd = Command::new(fixture_bin());
        cmd.env("PORT_FIXTURE_MODE", "env-port")
            .env_remove("PORT_FIXTURE_LOG")
            .env_remove("PORT")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut decoy = ChildGuard::new(&mut cmd);
        let rx = line_reader(decoy.0.stdout.take().unwrap());
        if let Ok(line) = rx.recv_timeout(Duration::from_secs(5)) {
            let port = app_line_port(&line);
            if port != u16::MAX && TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
                return (decoy, port, port + 1);
            }
        }
    }
    panic!("could not find a decoy port with a free successor");
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

/// Collects srvm's stdout until the `app` announcement, returning every line
/// seen plus the announcement itself. Callers inspect the earlier lines when
/// an internal retry may have printed more than one port report.
fn wait_announcement(rx: &mpsc::Receiver<String>, timeout: Duration) -> (Vec<String>, String) {
    let mut seen = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                let is_app = line.contains("app        http://127.0.0.1:");
                seen.push(line.clone());
                if is_app {
                    return (seen, line);
                }
            }
            Err(_) => panic!("timed out waiting for the app announcement"),
        }
    }
}

fn app_line_port(line: &str) -> u16 {
    line.rsplit("127.0.0.1:")
        .next()
        .unwrap()
        .trim_end_matches(|ch: char| !ch.is_ascii_digit())
        .parse()
        .unwrap()
}

fn http_get(port: u16) -> String {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

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
