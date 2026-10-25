//! Audit regression tests for srvm, pinning the documented guarantees the
//! v0.1.3 security audit (findings F-02, F-04, F-05, F-07) verified or fixed.
//!
//! Reading convention: a test is named for the *documented* behaviour it
//! pins. After the audit remediation every test in this file is green; a
//! failure means a guarantee regressed.
//!
//! Run everything:      cargo test --test audit_regressions
//!
//! No network access, no writes outside tempdirs, no listeners beyond
//! 127.0.0.1.
//!
//! F-03 (a `setsid()`-detaching descendant surviving Ctrl+C) is deliberately
//! *not* asserted here: it is a documented limitation — teardown is
//! best-effort over the process group, not a cgroup/Job Object — not a
//! guarantee to enforce. The group-level "resistant descendants are killed"
//! guarantee lives in `tests/lifecycle.rs`.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

// ---------------------------------------------------------------- helpers

/// A child that is always reaped, whatever the assertions do.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn stub(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn project(script_body: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("server.js"), script_body).unwrap();
    dir
}

#[cfg(unix)]
fn bin_dir() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    stub(dir.path(), "npm", "exit 0");
    dir
}

#[cfg(unix)]
fn srvm(path: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_srvm"));
    cmd.env("PATH", path)
        .env("NO_COLOR", "1")
        .env_remove("PORT")
        .arg("--no-open")
        .arg("--no-color")
        .arg("--no-install")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let entries: Vec<(&str, Option<&str>, &[u8])> =
        files.iter().map(|(p, b)| (*p, None, *b)).collect();
    tar_gz_entries(&entries)
}

fn tar_gz_symlink(entry: &str, target: &str) -> Vec<u8> {
    tar_gz_entries(&[(entry, Some(target), b"")])
}

/// Hand-rolled ustar writer: `tar::Builder` refuses `..` in member names, and
/// a `..` member is exactly what one of these tests needs to send.
fn tar_gz_entries(entries: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    fn header(name: &str, link: Option<&str>, size: usize, mode: u32, kind: u8) -> [u8; 512] {
        let mut block = [0u8; 512];
        let put = |block: &mut [u8; 512], at: usize, len: usize, src: &[u8]| {
            let n = src.len().min(len);
            block[at..at + n].copy_from_slice(&src[..n]);
        };
        put(&mut block, 0, 100, name.as_bytes());
        put(&mut block, 100, 8, format!("{mode:07o}\0").as_bytes());
        put(&mut block, 108, 8, b"0000000\0");
        put(&mut block, 116, 8, b"0000000\0");
        put(&mut block, 124, 12, format!("{size:011o}\0").as_bytes());
        put(&mut block, 136, 12, b"00000000000\0");
        block[156] = kind;
        if let Some(link) = link {
            put(&mut block, 157, 100, link.as_bytes());
        }
        put(&mut block, 257, 6, b"ustar\0");
        put(&mut block, 263, 2, b"00");
        // checksum: spaces while summing, then the octal result
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|byte| *byte as u32).sum();
        block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        block
    }

    let mut raw = Vec::new();
    for (name, link, body) in entries {
        raw.extend_from_slice(&header(
            name,
            *link,
            body.len(),
            if link.is_some() { 0o777 } else { 0o755 },
            if link.is_some() { b'2' } else { b'0' },
        ));
        raw.extend_from_slice(body);
        let pad = (512 - body.len() % 512) % 512;
        raw.extend(std::iter::repeat_n(0u8, pad));
    }
    raw.extend(std::iter::repeat_n(0u8, 1024));

    let mut gz = Vec::new();
    let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();
    gz
}

/// Minimal loopback HTTP file server for the runtime-fetch chain.
fn serve(routes: HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let path = req
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/")
                .to_string();
            let body = routes.get(&path).cloned();
            let (status, body) = match body {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", Vec::new()),
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A node fetch fixture: index.json, SHASUMS256.txt and the archive.
fn node_routes(archive: &[u8], digest: &str) -> HashMap<String, Vec<u8>> {
    let version = "v22.21.0";
    let target = srvm::runtime::node_target().unwrap();
    let filename = format!(
        "node-{version}-{target}.{}",
        srvm::runtime::node_archive_ext()
    );
    let mut routes = HashMap::new();
    routes.insert(
        "/index.json".into(),
        format!(r#"[{{"version":"{version}","lts":"Jod"}}]"#).into_bytes(),
    );
    routes.insert(
        format!("/{version}/SHASUMS256.txt"),
        format!("{digest}  {filename}\n").into_bytes(),
    );
    routes.insert(format!("/{version}/{filename}"), archive.to_vec());
    routes
}

/// Environment with PATH/HOME/cache scrubbed so a real toolchain cannot leak
/// in and satisfy the fetch from outside the fixture.
fn scrubbed(path: &Path, home: &Path, cache: &Path) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("srvm").unwrap();
    // Clear the whole environment rather than override individual variables:
    // on Windows an ambient toolchain leaks in through probes beyond PATH
    // (Program Files, the toolcache on `Path`, LOCALAPPDATA) and case-
    // insensitive environment lookup makes selective scrubbing unreliable.
    cmd.env_clear()
        .env("PATH", path)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SRVM_CACHE_DIR", cache);
    cmd
}

/// Everything the child emitted, for assertion failures.
fn show(out: &std::process::Output) -> String {
    format!(
        "exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// One raw HTTP exchange; nothing is normalised by a client library.
fn exchange(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    String::from_utf8_lossy(&raw).into_owned()
}

struct RawServer {
    _child: Guard,
    port: u16,
}

impl RawServer {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_srvm"))
            .arg(root)
            .args(["--no-open", "--no-color", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| panic!("spawn: {err}"))
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut seen = String::new();
        let mut buf = [0u8; 512];
        let deadline = Instant::now() + Duration::from_secs(20);
        let port = loop {
            assert!(Instant::now() < deadline, "no announcement: {seen}");
            let n = stdout.read(&mut buf).unwrap();
            assert!(n > 0, "server died: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if let Some(rest) = seen.split("127.0.0.1:").nth(1)
                && let Some(port) = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|digits| digits.parse().ok())
            {
                break port;
            }
        };
        // keep draining so the pipe never fills
        thread::spawn(move || {
            let mut sink = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut sink) {
                if n == 0 {
                    break;
                }
            }
        });
        Self {
            _child: Guard(child),
            port,
        }
    }

    // Only the (unix-only) symlinked-index test uses this convenience wrapper;
    // the cross-platform tests drive the server with `exchange` directly.
    #[cfg(unix)]
    fn get(&self, target: &str) -> String {
        exchange(
            self.port,
            &format!(
                "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                self.port
            ),
        )
    }
}

fn status_of(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

fn site(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    for (rel, body) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    dir
}

// ------------------------------------------------------- defects (red)

/// TECH-SPEC "Design rules": "Exit honesty. … the child's original nonzero
/// code on failure."
#[cfg(unix)]
#[test]
fn documented_child_exit_code_is_preserved() {
    let bin = tempdir().unwrap();
    stub(bin.path(), "npm", "echo boom >&2\nexit 7");
    let repo = project("");
    let out = srvm(bin.path())
        .arg(repo.path())
        .arg("--port")
        .arg("0")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("exit status: 7"),
        "child reported 7: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "srvm must exit with the child's code, not a generic 1: {stderr}"
    );
}

/// RFC 9112 §3.2.2 absolute-form request target.
#[test]
fn documented_absolute_form_request_target_is_served() {
    let root = site(&[("index.html", b"ok")]);
    let server = RawServer::start(root.path());
    let response = exchange(
        server.port,
        &format!(
            "GET http://127.0.0.1:{}/ HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            server.port, server.port
        ),
    );
    assert_eq!(status_of(&response), 200, "{response}");
}

/// A browser opened on the IPv6 loopback sends an `[::1]` authority, and
/// srvm's own URL regexes treat `[::1]` as a loopback form.
#[test]
fn documented_ipv6_loopback_host_is_accepted() {
    let root = site(&[("index.html", b"ok")]);
    let server = RawServer::start(root.path());
    let response = exchange(
        server.port,
        &format!(
            "GET / HTTP/1.1\r\nHost: [::1]:{}\r\nConnection: close\r\n\r\n",
            server.port
        ),
    );
    assert_eq!(status_of(&response), 200, "{response}");
}

/// A fully-qualified `127.0.0.1.` is the same authority.
#[test]
fn documented_trailing_dot_host_is_accepted() {
    let root = site(&[("index.html", b"ok")]);
    let server = RawServer::start(root.path());
    let response = exchange(
        server.port,
        &format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1.:{}\r\nConnection: close\r\n\r\n",
            server.port
        ),
    );
    assert_eq!(status_of(&response), 200, "{response}");
}

/// An empty `Host:` value is a malformed request (400), not a forbidden one.
#[test]
fn empty_host_is_a_bad_request() {
    let root = site(&[("index.html", b"ok")]);
    let server = RawServer::start(root.path());
    let response = exchange(
        server.port,
        "GET / HTTP/1.1\r\nHost: \r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status_of(&response), 400, "{response}");
}

/// Discovery accepts a symlinked `index.html` (`is_file` follows links) and
/// the dry run promises the site; the server then 404s every request because
/// `serve_path` refuses symlinks.
#[cfg(unix)]
#[test]
fn discovered_symlinked_index_is_actually_served() {
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("index.html"), b"outside-index").unwrap();
    let root = tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("index.html"),
        root.path().join("index.html"),
    )
    .unwrap();

    let dry = srvm(bin_dir().path())
        .arg("--dry-run")
        .arg(root.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        stdout.contains("match      1. static"),
        "discovery selected the site: {stdout}"
    );

    let server = RawServer::start(root.path());
    let response = server.get("/");
    assert_eq!(
        status_of(&response),
        200,
        "a site discovery accepted must be servable: {response}"
    );
}

// ------------------------------------------------- guarantees (green)

/// `--no-install` skips bootstrap commands but still provisions a runtime
/// into the cache. README says "never install anything"; runtime_boot.rs
/// already relies on the current behaviour.
#[test]
fn no_install_still_downloads_a_missing_runtime() {
    let version = "v22.21.0";
    let target = srvm::runtime::node_target().unwrap();
    let filename = format!(
        "node-{version}-{target}.{}",
        srvm::runtime::node_archive_ext()
    );
    let tool_rel = if cfg!(windows) {
        format!("node-{version}-{target}/node.exe")
    } else {
        format!("node-{version}-{target}/bin/node")
    };
    let archive = tar_gz(&[(&tool_rel, b"#!/bin/sh\nexit 0\n")]);
    let base = serve(node_routes(&archive, &sha_hex(&archive)));

    let repo = project("");
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let out = scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .arg("--no-open")
        .arg("--no-install")
        .arg("--port")
        .arg("0")
        .arg(repo.path())
        .output()
        .unwrap();

    let installed = cache
        .path()
        .join("runtimes")
        .join("node")
        .join(version)
        .join(".srvm-ok");
    assert!(
        installed.is_file(),
        "a runtime was downloaded and published despite --no-install: {filename}\n{}",
        show(&out)
    );
}

/// Dry run is pure: no bootstrap, no fetch, no cache writes, no tree churn.
#[test]
fn dry_run_writes_nothing_anywhere() {
    let repo = tempdir().unwrap();
    std::fs::write(repo.path().join("manage.py"), "").unwrap();
    std::fs::write(repo.path().join("requirements.txt"), "django\n").unwrap();
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    scrubbed(path.path(), home.path(), cache.path())
        .arg("--dry-run")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("install    python3 -m venv .venv"));

    assert!(
        !repo.path().join(".venv").exists(),
        "venv must not be created"
    );
    assert!(
        cache.path().read_dir().unwrap().next().is_none(),
        "cache must stay empty on a dry run"
    );
}

/// `.env` is parse-only and the ambient environment wins.
#[cfg(unix)]
#[test]
fn dotenv_is_parse_only_and_ambient_wins() {
    let bin = tempdir().unwrap();
    let scratch = tempdir().unwrap();
    let dump = scratch.path().join("env.txt");
    stub(
        bin.path(),
        "npm",
        &format!(
            "env > '{}'\nprintf 'app http://127.0.0.1:9/\\n'",
            dump.display()
        ),
    );
    let repo = project("");
    std::fs::write(
        repo.path().join(".env"),
        "CMD=$(id -un)\nOVERRIDE=file\nBROWSER=firefox\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_srvm"))
        .arg(repo.path())
        .args(["--no-open", "--no-color", "--no-install", "--port", "0"])
        .env("PATH", format!("{}:/bin:/usr/bin", bin.path().display()))
        .env("OVERRIDE", "shell")
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dump.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();

    let text = std::fs::read_to_string(&dump).unwrap();
    assert!(
        text.contains("CMD=$(id -un)"),
        "no command substitution: {text}"
    );
    assert!(text.contains("OVERRIDE=shell"), "ambient must win: {text}");
    assert!(text.contains("BROWSER=none"), "BROWSER is forced: {text}");
}

/// A script name reaches argv verbatim, as one element.
#[cfg(unix)]
#[test]
fn script_name_reaches_argv_verbatim() {
    let bin = tempdir().unwrap();
    let scratch = tempdir().unwrap();
    let dumped = scratch.path().join("argv.txt");
    stub(
        bin.path(),
        "npm",
        &format!(
            "printf '[%s]' \"$@\" > '{}'\nprintf 'app http://127.0.0.1:9/\\n'",
            dumped.display()
        ),
    );
    let repo = tempdir().unwrap();
    std::fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev:x y;z":"node s.js"}}"#,
    )
    .unwrap();

    let _ = srvm(bin.path())
        .arg(repo.path())
        .args(["--port", "0"])
        .output()
        .unwrap();
    let argv = std::fs::read_to_string(&dumped).unwrap();
    assert_eq!(argv, "[run][dev:x y;z]", "unquoted by any shell");
}

/// A tar member outside the destination is refused, not silently written.
#[test]
fn archive_member_with_parent_dir_is_refused() {
    let escaped = tempdir().unwrap();
    let archive = tar_gz(&[
        ("pkg/bin/node", b"#!/bin/sh\nexit 0\n"),
        ("../srvm-escape-probe.txt", b"escaped\n"),
    ]);
    let base = serve(node_routes(&archive, &sha_hex(&archive)));
    let repo = project("");
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    let out = scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .args(["--no-open", "--no-install", "--port", "0"])
        .arg(repo.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("refusing archive entry"), "{}", show(&out));
    assert!(
        !cache
            .path()
            .join("runtimes/node/v22.21.0/.srvm-ok")
            .exists()
    );
    drop(escaped);
}

/// A symlink whose target leaves the destination is refused.
#[test]
fn archive_escaping_symlink_is_refused() {
    let archive = tar_gz_symlink("bin/node", "/etc/passwd");
    let base = serve(node_routes(&archive, &sha_hex(&archive)));
    let repo = project("");
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    let out = scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .args(["--no-open", "--no-install", "--port", "0"])
        .arg(repo.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("refusing symlink"), "{}", show(&out));
}

/// A checksum mismatch is detected before anything is unpacked or published.
#[test]
fn checksum_mismatch_publishes_nothing() {
    let archive = tar_gz(&[("pkg/bin/node", b"#!/bin/sh\nexit 0\n")]);
    let base = serve(node_routes(&archive, &"0".repeat(64)));
    let repo = project("");
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    let out = scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .args(["--no-open", "--no-install", "--port", "0"])
        .arg(repo.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("checksum mismatch"), "{}", show(&out));
    assert!(
        !cache
            .path()
            .join("runtimes/node/v22.21.0/.srvm-ok")
            .exists()
    );
}

/// No CORS header is sent, which is what makes cross-origin reads from a web
/// page impossible. Documented so the decision is not "fixed" by surprise.
#[test]
fn static_server_sends_no_cors_header() {
    let root = site(&[("index.html", b"ok")]);
    let server = RawServer::start(root.path());
    let response = exchange(
        server.port,
        &format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://evil.test\r\nConnection: close\r\n\r\n",
            server.port
        ),
    );
    assert_eq!(status_of(&response), 200);
    assert!(
        !response
            .to_ascii_lowercase()
            .contains("access-control-allow-origin"),
        "{response}"
    );
}
