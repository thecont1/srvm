//! Bootstrap installs against a real interpreter.
//!
//! These are the only tests that create a real virtualenv, so they skip with a
//! message when no interpreter is installed. The requirement list is empty, so
//! nothing is downloaded.

use std::{
    env, fs,
    path::Path,
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

use tempfile::{TempDir, tempdir};

mod support;
use support::*;

const MANAGE_PY: &str = r#"import http.server
import socketserver
import sys

port = int(sys.argv[-1])


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


with socketserver.TCPServer(("127.0.0.1", port), Handler) as httpd:
    print(f"ready on http://127.0.0.1:{port}/", flush=True)
    httpd.serve_forever()
"#;

fn real_python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|name| {
        Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn django_repo() -> TempDir {
    let repo = tempdir().unwrap();
    fs::write(repo.path().join("manage.py"), MANAGE_PY).unwrap();
    fs::write(repo.path().join("requirements.txt"), "# no dependencies\n").unwrap();
    repo
}

/// The real interpreter must be visible, so this deliberately does not use the
/// stub-PATH helper.
fn srvm_with_real_path(repo: &Path) -> ChildGuard {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_srvm"));
    cmd.env("NO_COLOR", "1")
        .env_remove("PORT")
        .args(["--no-open", "--no-color", "--port", "0"])
        .arg(repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    ChildGuard::new(&mut cmd)
}

#[test]
fn python_bootstrap_creates_a_venv_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(_python) = real_python() else {
        eprintln!("skipping: no python3/python on PATH to build a virtualenv");
        return;
    };
    let repo = django_repo();

    let mut child = srvm_with_real_path(repo.path());
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    let (lines, line) = collect_until(&out, &err, "app        ", Duration::from_secs(120));

    assert!(
        lines.iter().any(|line| line.contains("-m venv .venv")),
        "the virtualenv is created: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("pip install -r requirements.txt")),
        "requirements are installed with the venv's own pip: {lines:?}"
    );
    assert!(
        repo.path().join(".venv/.srvm-bootstrap").is_file(),
        "a successful bootstrap is stamped"
    );

    assert!(http_get(app_line_port(&line)).contains("200 OK"));
    stop(&mut child.0);

    // A second run must not repeat the work the stamp already covers.
    let mut again = srvm_with_real_path(repo.path());
    let out = line_reader(again.0.stdout.take().unwrap());
    let err = line_reader(again.0.stderr.take().unwrap());
    let (lines, line) = collect_until(&out, &err, "app        ", Duration::from_secs(120));

    assert!(
        !lines
            .iter()
            .any(|line| line.contains("installing dependencies")),
        "a fresh stamp means no install: {lines:?}"
    );
    assert!(http_get(app_line_port(&line)).contains("200 OK"));
    stop(&mut again.0);
}

/// Collects stdout until `needle` arrives, returning everything seen (including
/// the needle line) so a test can assert on the lines that preceded it.
fn collect_until(
    rx: &mpsc::Receiver<String>,
    err: &mpsc::Receiver<String>,
    needle: &str,
    timeout: Duration,
) -> (Vec<String>, String) {
    let deadline = Instant::now() + timeout;
    let mut lines = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) if line.contains(needle) => {
                lines.push(line.clone());
                return (lines, line);
            }
            Ok(line) => lines.push(line),
            Err(_) => {
                let stderr = drain(err, Duration::from_secs(2)).join("\n");
                panic!(
                    "timed out waiting for {needle:?}; srvm stdout:\n{}\nsrvm stderr:\n{stderr}",
                    lines.join("\n")
                );
            }
        }
    }
}

/// The generated Django stand-in serves until it is stopped, unlike the port
/// fixture, so each launch ends here.
fn stop(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}
