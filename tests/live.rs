//! Opt-in live suite: real upstream archives and real toolchain boots.
//!
//! These tests deliberately break the fixture hermeticism: they download
//! actual Node/Python/Go/Rust distributions, install real packages, and boot
//! real frameworks. They exist to prove the M7 gate claims — fixtures are not
//! upstream proof — and are never part of the default test run.
//!
//! Run explicitly: `cargo test --locked --features live --test live`.
//!
//! Isolation rules (per the M7 gate):
//! - `HOME`/`USERPROFILE` point at a fresh temp dir, so home-shim detection
//!   (.pyenv, .volta, ...) cannot leak host runtimes into srvm's view.
//! - `PATH` is a synthetic "farm" of symlinks to *infrastructure* tools only
//!   (sh, cc, ld, make, ...). No directory that ships a language runtime is
//!   ever on this PATH, so srvm's detection cannot find a host node/python/go/
//!   cargo and must walk its own fetch path.
//! - `SRVM_CACHE_DIR` is a fresh temp dir per test; nothing is shared.
//! - Host linkers/SDKs are intentionally retained (the farm links the system
//!   cc/ld): the gate tests srvm's runtime fetch, not a cross SDK.
//!
//! Every test asserts two things: the app really served (HTTP 200 on the
//! announced port), and the runtime really was fetched (the isolated cache
//! contains the expected runtime tree).

#![cfg(feature = "live")]

mod support;

use std::{env, fs, path::Path, process::Command, time::Duration};

#[cfg(windows)]
use std::path::PathBuf;

use support::{ChildGuard, SERIAL, app_line_port, http_get, line_reader, wait_for_all};
use tempfile::{TempDir, tempdir};

/// How long a live boot may take end to end. Rust downloads the largest
/// archives and compiles real code; every other runtime gets a tighter bound.
const RUST_BOOT: Duration = Duration::from_secs(25 * 60);
const BOOT: Duration = Duration::from_secs(15 * 60);

fn srvm_bin() -> &'static str {
    env!("CARGO_BIN_EXE_srvm")
}

/// A PATH made of symlinks to infrastructure tools only. srvm and its apps
/// can spawn sh/cc/ld/make, but no node/python/go/cargo binary exists on
/// this PATH, so the fetch path cannot be bypassed by a host runtime.
fn build_farm() -> TempDir {
    let farm = tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let sys = ["/usr/bin", "/bin"];
        let tools: &[(&str, &[&str])] = &[
            ("sh", &["sh"]),
            ("bash", &["bash"]),
            ("cc", &["cc", "gcc", "clang"]),
            ("clang", &["clang"]),
            ("gcc", &["gcc"]),
            ("g++", &["g++"]),
            ("ld", &["ld"]),
            ("ar", &["ar"]),
            ("as", &["as"]),
            ("strip", &["strip"]),
            ("make", &["make"]),
        ];
        for (name, candidates) in tools {
            for dir in sys {
                for candidate in *candidates {
                    let src = Path::new(dir).join(candidate);
                    if src.is_file() {
                        let _ = symlink(&src, farm.path().join(name));
                        break;
                    }
                }
                if farm.path().join(name).exists() {
                    break;
                }
            }
        }
    }
    farm
}

/// The environment srvm and everything it spawns live in: scrubbed PATH,
/// isolated home/cache/temp, no host-runtime visibility.
fn live_env(cmd: &mut Command, farm: &Path, home: &Path, cache: &Path) {
    cmd.env_clear();
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("PATH", path_var(farm));
    cmd.env("NO_COLOR", "1");
    cmd.env("SRVM_CACHE_DIR", cache);
    cmd.env("HOME", home);
    cmd.env("TEMP", home);
    cmd.env("TMP", home);
    #[cfg(windows)]
    {
        cmd.env("USERPROFILE", home);
        cmd.env("LOCALAPPDATA", home);
        if let Some(root) = env::var_os("SystemRoot") {
            cmd.env("SystemRoot", root);
        }
    }
}

#[cfg(windows)]
fn path_var(farm: &Path) -> std::ffi::OsString {
    let mut entries = vec![farm.to_path_buf()];
    if let Some(root) = env::var_os("SystemRoot") {
        entries.push(PathBuf::from(&root).join("System32"));
    }
    env::join_paths(entries).unwrap()
}

#[cfg(not(windows))]
fn path_var(farm: &Path) -> std::ffi::OsString {
    env::join_paths([farm.to_path_buf()]).unwrap()
}

/// Everything a live boot needs, kept alive for the whole test: the child,
/// the announced URLs, and the isolated home/cache/farm directories.
struct Boot {
    // Kept alive until the test ends; Drop is the teardown guard.
    _child: ChildGuard,
    urls: Vec<String>,
    cache: TempDir,
    _home: TempDir,
    _farm: TempDir,
}

/// Spawn srvm against `repo` in the isolated environment and wait for every
/// app's announcement line.
fn boot(repo: &Path, timeout: Duration) -> Boot {
    let farm = build_farm();
    let home = tempdir().unwrap();
    let cache = tempdir().unwrap();
    let mut cmd = Command::new(srvm_bin());
    live_env(&mut cmd, farm.path(), home.path(), cache.path());
    cmd.arg(repo).arg("--no-open").arg("--no-color");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    let urls = wait_for_all(&out, &err, &["http://127.0.0.1:"], timeout);
    Boot {
        _child: child,
        urls,
        cache,
        _home: home,
        _farm: farm,
    }
}

/// The isolated cache must contain the runtime tree for `kind` — proof the
/// boot ran on fetched runtimes, not host tools.
fn assert_fetched(cache: &Path, kind: &str) {
    let tree = cache.join("runtimes").join(kind);
    assert!(
        tree.is_dir(),
        "expected a fetched {kind} tree under {}; the boot must not have \
         used a host runtime",
        tree.display()
    );
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

// ---- real repositories ----------------------------------------------------

fn node_repo(with_dep: bool) -> TempDir {
    let repo = tempdir().unwrap();
    let dep = if with_dep {
        r#","dependencies":{"is-number":"^7.0.0"}"#
    } else {
        ""
    };
    write(
        repo.path(),
        "package.json",
        &format!(r#"{{"name":"live","scripts":{{"dev":"node index.js"}}{dep}}}"#),
    );
    write(
        repo.path(),
        "index.js",
        r#"const http = require("http");
const server = http.createServer((req, res) => { res.end("ok"); });
server.listen(Number(process.env.PORT || 0), "127.0.0.1", () => {
  console.log(`ready on http://127.0.0.1:${server.address().port}/`);
});"#,
    );
    repo
}

fn django_repo() -> TempDir {
    let repo = tempdir().unwrap();
    write(repo.path(), "requirements.txt", "Django>=5.2,<5.3\n");
    write(
        repo.path(),
        "manage.py",
        r#"#!/usr/bin/env python
import os, sys
def main():
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
    from django.core.management import execute_from_command_line
    execute_from_command_line(sys.argv)
if __name__ == "__main__":
    main()
"#,
    );
    write(repo.path(), "mysite/__init__.py", "");
    write(
        repo.path(),
        "mysite/settings.py",
        r#"SECRET_KEY = "live-test-only"
DEBUG = True
ALLOWED_HOSTS = ["*"]
ROOT_URLCONF = "mysite.urls"
INSTALLED_APPS = ["django.contrib.staticfiles"]
MIDDLEWARE = []
DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
"#,
    );
    write(
        repo.path(),
        "mysite/urls.py",
        r#"from django.http import HttpResponse
from django.urls import path
urlpatterns = [path("", lambda request: HttpResponse("ok"))]
"#,
    );
    repo
}

fn flask_repo() -> TempDir {
    let repo = tempdir().unwrap();
    write(repo.path(), "requirements.txt", "Flask>=3.0,<4\n");
    write(
        repo.path(),
        "app.py",
        r#"from flask import Flask
app = Flask(__name__)
@app.route("/")
def index():
    return "ok"
"#,
    );
    repo
}

fn go_repo() -> TempDir {
    let repo = tempdir().unwrap();
    write(repo.path(), "go.mod", "module live\n\ngo 1.21\n");
    write(
        repo.path(),
        "main.go",
        r#"package main

import (
	"fmt"
	"net/http"
	"os"
)

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		port = "0"
	}
	listener, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		panic(err)
	}
	fmt.Printf("ready on http://127.0.0.1:%d/\n", listener.Addr().(*net.TCPAddr).Port)
	http.Serve(listener, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("ok"))
	}))
}
"#,
    );
    repo
}

fn rust_repo() -> TempDir {
    let repo = tempdir().unwrap();
    write(
        repo.path(),
        "Cargo.toml",
        r#"[package]
name = "live"
version = "0.1.0"
edition = "2021"
"#,
    );
    write(
        repo.path(),
        "src/main.rs",
        r#"use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let actual = listener.local_addr().unwrap().port();
    println!("ready on http://127.0.0.1:{actual}/");
    loop {
        let (mut stream, _) = listener.accept().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        });
    }
}
"#,
    );
    repo
}

// ---- the smokes -----------------------------------------------------------

#[test]
fn real_rust_archives_compile_and_serve() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = rust_repo();
    let boot = boot(repo.path(), RUST_BOOT);
    for line in &boot.urls {
        assert!(http_get(app_line_port(line)).contains("200"));
    }
    assert_fetched(boot.cache.path(), "rust");
}

#[test]
fn real_node_package_manager_boot() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = node_repo(true);
    let boot = boot(repo.path(), BOOT);
    for line in &boot.urls {
        assert!(http_get(app_line_port(line)).contains("200"));
    }
    assert_fetched(boot.cache.path(), "node");
}

#[test]
fn real_python_venv_and_django_boot() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = django_repo();
    let boot = boot(repo.path(), BOOT);
    for line in &boot.urls {
        assert!(http_get(app_line_port(line)).contains("200"));
    }
    assert_fetched(boot.cache.path(), "python");
}

#[test]
fn real_go_compile_and_serve() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = go_repo();
    let boot = boot(repo.path(), BOOT);
    for line in &boot.urls {
        assert!(http_get(app_line_port(line)).contains("200"));
    }
    assert_fetched(boot.cache.path(), "go");
}

#[test]
fn nested_all_boots_two_real_apps() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempdir().unwrap();
    let frontend = node_repo(false);
    let backend = flask_repo();
    // A bare root with conventional sub-apps; --all must boot both.
    fs::rename(frontend.path(), root.path().join("frontend")).unwrap();
    fs::rename(backend.path(), root.path().join("backend")).unwrap();

    let farm = build_farm();
    let home = tempdir().unwrap();
    let cache = tempdir().unwrap();
    let mut cmd = Command::new(srvm_bin());
    live_env(&mut cmd, farm.path(), home.path(), cache.path());
    cmd.arg(root.path())
        .arg("--all")
        .arg("--no-open")
        .arg("--no-color");
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    let urls = wait_for_all(&out, &err, &["http://127.0.0.1:"], BOOT);
    // Two apps must have announced; wait_for_all proves only one line here,
    // so keep reading until a second distinct URL appears.
    let urls = distinct_urls(out, err, urls, BOOT);
    for line in &urls {
        assert!(http_get(app_line_port(line)).contains("200"));
    }
    assert_fetched(cache.path(), "node");
    assert_fetched(cache.path(), "python");
}

/// Read lines until `n` distinct announced URLs have been seen (the first
/// needle match counts as one). Panics on timeout with both streams.
fn distinct_urls(
    out: std::sync::mpsc::Receiver<String>,
    err: std::sync::mpsc::Receiver<String>,
    mut seen: Vec<String>,
    timeout: Duration,
) -> Vec<String> {
    use std::sync::mpsc::RecvTimeoutError;
    let deadline = std::time::Instant::now() + timeout;
    while seen.len() < 2 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match out.recv_timeout(remaining) {
            Ok(line) if line.contains("http://127.0.0.1:") => {
                let port = app_line_port(&line);
                if !seen.iter().any(|s| app_line_port(s) == port) {
                    seen.push(line);
                }
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for two announced URLs"),
            Err(RecvTimeoutError::Disconnected) => {
                let stderr = support::drain(&err, Duration::from_secs(2)).join("\n");
                panic!("srvm exited before two apps announced; stderr:\n{stderr}");
            }
        }
    }
    seen
}
