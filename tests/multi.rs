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
fn all_launches_two_apps_with_distinct_labeled_urls() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let js_line = wait_for(
        &out,
        &err,
        "app        [package:dev] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let py_line = wait_for(
        &out,
        &err,
        "app        [django] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let js_port = app_line_port(&js_line);
    let py_port = app_line_port(&py_line);
    assert_ne!(js_port, py_port);

    assert!(http_get(js_port).starts_with("HTTP/1.1 200"));
    assert!(http_get(py_port).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn all_dry_run_lists_launch_set_without_spawning() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));
    let log = repo.path().join("fixture.log");

    let output = srvm(bin.path())
        .env("PORT_FIXTURE_LOG", &log)
        .arg("--all")
        .arg("--dry-run")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("launch     1. package:dev"), "{stdout}");
    assert!(stdout.contains("launch     2. django"), "{stdout}");

    assert!(!repo.path().join("srvm-fixture-install.txt").exists());
    assert!(!log.exists());
}

#[test]
fn all_conflicts_with_select() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec(bin.path(), "npm");
    stub_exec(bin.path(), "python3");

    let output = srvm(bin.path())
        .arg("--all")
        .arg("--select")
        .arg("1")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--all"), "{stderr}");
    assert!(stderr.contains("--select"), "{stderr}");
}

#[test]
fn all_with_one_app_runs_single_path() {
    let repo = package_only_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let app_line = wait_for(
        &out,
        &err,
        "app        http://127.0.0.1:",
        Duration::from_secs(30),
    );
    assert!(
        !app_line.contains('['),
        "single app must be unlabeled: {app_line}"
    );
    assert!(http_get(app_line_port(&app_line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn all_sibling_failure_tears_down_the_rest() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("fail-after-hold"));
    let release = repo.path().join("release");

    let mut cmd = srvm(bin.path());
    cmd.env("PORT_FIXTURE_RELEASE", &release);
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let js_line = wait_for(
        &out,
        &err,
        "app        [package:dev] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let js_port = app_line_port(&js_line);

    fs::write(&release, "fail").unwrap();
    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert!(
        !status.success(),
        "sibling failure must exit non-zero: {status}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpListener::bind(("127.0.0.1", js_port)) {
            Ok(listener) => {
                drop(listener);
                break;
            }
            Err(err) if Instant::now() < deadline => {
                let _ = err;
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => panic!("npm app port still held after teardown: {err}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn all_ctrl_c_stops_every_child() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let js_line = wait_for(
        &out,
        &err,
        "app        [package:dev] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let py_line = wait_for(
        &out,
        &err,
        "app        [django] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let ports = [app_line_port(&js_line), app_line_port(&py_line)];

    unsafe {
        libc::kill(child.0.id() as libc::pid_t, libc::SIGINT);
    }
    let status = wait_exit(&mut child.0, Duration::from_secs(10));
    assert_eq!(status.code(), Some(130), "SIGINT must exit 130: {status}");

    for port in ports {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => {
                    drop(listener);
                    break;
                }
                Err(err) if Instant::now() < deadline => {
                    let _ = err;
                    thread::sleep(Duration::from_millis(100));
                }
                Err(err) => panic!("port {port} still held after SIGINT: {err}"),
            }
        }
    }
}

#[test]
fn all_port_start_allocates_distinct_ascending_ports() {
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));
    let (_held, start) = free_with_free_next();

    let mut cmd = srvm(bin.path());
    cmd.arg("--all")
        .arg("--port")
        .arg(start.to_string())
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let js_line = wait_for(
        &out,
        &err,
        "app        [package:dev] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let py_line = wait_for(
        &out,
        &err,
        "app        [django] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let js_port = app_line_port(&js_line);
    let py_port = app_line_port(&py_line);

    assert!(js_port >= start, "{js_port} below requested {start}");
    assert!(py_port > js_port, "{py_port} must follow {js_port}");

    assert!(http_get(js_port).starts_with("HTTP/1.1 200"));
    assert!(http_get(py_port).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

fn two_app_repo() -> TempDir {
    let repo = package_only_repo();
    fs::write(repo.path().join("manage.py"), "").unwrap();
    repo
}

fn package_only_repo() -> TempDir {
    let repo = tempdir().unwrap();
    fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();
    repo
}

fn srvm(bin: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_srvm"));
    cmd.env("PATH", bin)
        .env_remove("PORT")
        .env_remove("PORT_FIXTURE_MODE")
        .env_remove("PORT_FIXTURE_LOG")
        .env_remove("PORT_FIXTURE_RELEASE")
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
    stub_exec_mode(dir, name, None);
}

fn stub_exec_mode(dir: &Path, name: &str, mode: Option<&str>) {
    let fixture = fixture_bin();

    #[cfg(windows)]
    {
        let export = mode
            .map(|mode| format!("set PORT_FIXTURE_MODE={mode}\r\n"))
            .unwrap_or_default();
        fs::write(
            dir.join(format!("{name}.cmd")),
            format!(
                "@echo off\r\n{export}\"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
                fixture.display()
            ),
        )
        .unwrap();
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let export = mode
            .map(|mode| format!("PORT_FIXTURE_MODE={mode} "))
            .unwrap_or_default();
        let path = dir.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\n{export}exec \"{}\" \"$@\"\n", fixture.display()),
        )
        .unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }
}

fn free_with_free_next() -> (TcpListener, u16) {
    for _ in 0..25 {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        if port == u16::MAX {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return (held, port);
        }
    }
    panic!("could not find a free port with a free successor");
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

fn drain(rx: &mpsc::Receiver<String>, timeout: Duration) -> Vec<String> {
    let mut lines = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return lines;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => lines.push(line),
            Err(_) => return lines,
        }
    }
}

/// Like `wait_line` but the timeout panic includes both streams, so a failed
/// launch reports srvm's stderr instead of a bare timeout.
fn wait_for(
    rx: &mpsc::Receiver<String>,
    err: &mpsc::Receiver<String>,
    needle: &str,
    timeout: Duration,
) -> String {
    let mut seen = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) if line.contains(needle) => return line,
            Ok(line) => seen.push(line),
            Err(_) => {
                let stderr = drain(err, Duration::from_secs(2)).join("\n");
                panic!(
                    "timed out waiting for output containing {needle:?}; srvm stdout:\n{}\nsrvm stderr:\n{stderr}",
                    seen.join("\n")
                );
            }
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
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
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
