//! Shared fixtures and process helpers for the CLI integration tests.
#![allow(dead_code)]

use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use tempfile::{TempDir, tempdir};

// Launching tests run serialized: `--port 0` hands sibling tests' fixtures the
// same ephemeral ports faster than the bounded handoff retry absorbs.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub fn srvm(bin: &Path) -> Command {
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

pub fn fixture_bin() -> PathBuf {
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

pub fn stub_exec(dir: &Path, name: &str) {
    stub_exec_mode(dir, name, None);
}

pub fn stub_exec_mode(dir: &Path, name: &str, mode: Option<&str>) {
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

pub fn free_with_free_next() -> (TcpListener, u16) {
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

pub fn line_reader(stream: impl Read + Send + 'static) -> mpsc::Receiver<String> {
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

pub fn drain(rx: &mpsc::Receiver<String>, timeout: Duration) -> Vec<String> {
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

/// Waits until every needle has matched a distinct line, returning the
/// matching line per needle in input order. Announcements can arrive in any
/// order, so sequential `wait_for` calls would lose a later needle's line to
/// the first call's buffer.
pub fn wait_for_all(
    rx: &mpsc::Receiver<String>,
    err: &mpsc::Receiver<String>,
    needles: &[&str],
    timeout: Duration,
) -> Vec<String> {
    let mut seen = Vec::new();
    let mut matched = vec![None; needles.len()];
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                for (idx, needle) in needles.iter().enumerate() {
                    if matched[idx].is_none() && line.contains(needle) {
                        matched[idx] = Some(line.clone());
                    }
                }
                seen.push(line);
                if matched.iter().all(Option::is_some) {
                    return matched.into_iter().map(Option::unwrap).collect();
                }
            }
            Err(_) => {
                let stderr = drain(err, Duration::from_secs(2)).join("\n");
                panic!(
                    "timed out waiting for all of {needles:?}; srvm stdout:\n{}\nsrvm stderr:\n{stderr}",
                    seen.join("\n")
                );
            }
        }
    }
}

/// Like `wait_for_all`, but also returns every line that arrived, including the
/// narration (installs, handoff retries) that precedes the matched lines.
pub fn wait_for_all_seen(
    rx: &mpsc::Receiver<String>,
    err: &mpsc::Receiver<String>,
    needles: &[&str],
    timeout: Duration,
) -> (Vec<String>, Vec<String>) {
    let mut seen = Vec::new();
    let mut matched = vec![None; needles.len()];
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                for (idx, needle) in needles.iter().enumerate() {
                    if matched[idx].is_none() && line.contains(needle) {
                        matched[idx] = Some(line.clone());
                    }
                }
                seen.push(line);
                if matched.iter().all(Option::is_some) {
                    let matched = matched.into_iter().map(Option::unwrap).collect();
                    return (seen, matched);
                }
            }
            Err(_) => {
                let stderr = drain(err, Duration::from_secs(2)).join("\n");
                panic!(
                    "timed out waiting for all of {needles:?}; srvm stdout:\n{}\nsrvm stderr:\n{stderr}",
                    seen.join("\n")
                );
            }
        }
    }
}

/// Like `wait_line` but the timeout panic includes both streams, so a failed
/// launch reports srvm's stderr instead of a bare timeout.
pub fn wait_for(
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

pub fn app_line_port(line: &str) -> u16 {
    line.rsplit("127.0.0.1:")
        .next()
        .unwrap()
        .trim_end_matches(|ch: char| !ch.is_ascii_digit())
        .parse()
        .unwrap()
}

pub fn http_get(port: u16) -> String {
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

pub fn wait_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("child did not exit within {timeout:?}");
}

pub struct ChildGuard(pub Child);

impl ChildGuard {
    pub fn new(cmd: &mut Command) -> Self {
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
    pub fn wait_bounded(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.0.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}
