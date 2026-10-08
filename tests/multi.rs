use std::{
    fs,
    net::TcpListener,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use tempfile::{TempDir, tempdir};

mod support;
use support::*;

#[test]
fn all_launches_two_apps_with_distinct_labeled_urls() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let lines = wait_for_all(
        &out,
        &err,
        &[
            "app        [package:dev] http://127.0.0.1:",
            "app        [django] http://127.0.0.1:",
        ],
        Duration::from_secs(30),
    );
    let js_port = app_line_port(&lines[0]);
    let py_port = app_line_port(&lines[1]);
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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--all").arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let lines = wait_for_all(
        &out,
        &err,
        &[
            "app        [package:dev] http://127.0.0.1:",
            "app        [django] http://127.0.0.1:",
        ],
        Duration::from_secs(30),
    );
    let ports = [app_line_port(&lines[0]), app_line_port(&lines[1])];

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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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

    let (seen, lines) = wait_for_all_seen(
        &out,
        &err,
        &[
            "app        [package:dev] http://127.0.0.1:",
            "app        [django] http://127.0.0.1:",
        ],
        Duration::from_secs(30),
    );
    let js_port = app_line_port(&lines[0]);
    let py_port = app_line_port(&lines[1]);

    assert!(js_port >= start, "{js_port} below requested {start}");
    assert!(py_port >= start, "{py_port} below requested {start}");
    assert_ne!(js_port, py_port, "ports must stay distinct");
    // Ascending allocation describes the initial selection. If a port was
    // stolen in the handoff window, the retried app moves above every port the
    // launch selected, so launch order no longer implies port order and only
    // distinctness is promised.
    if !seen.iter().any(|line| line.contains("retrying")) {
        assert!(py_port > js_port, "{py_port} must follow {js_port}");
    }

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

/// A stolen port must never push the retried app onto a sibling's port.
///
/// Both children are held before they bind. The test steals the first app's
/// reserved port and lets only that app proceed, so its retry happens while the
/// sibling is still waiting to bind — exactly the window in which a naive
/// `selected + 1` retry would land on the sibling's port and make the sibling
/// fail and retry too. The launch must instead move the retried app clear of
/// every port this launch selected.
#[test]
fn all_handoff_retry_never_steals_a_sibling_port() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = two_app_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    // The django app takes its port from argv and gets its own release file, so
    // it stays unbound while the npm app retries.
    let py_release = repo.path().join("py-release");
    write_python_stub(bin.path(), &py_release);
    let release = repo.path().join("release");
    let (_held, start) = free_with_free_next();
    let npm_reserved = start + 1;
    let py_reserved = start + 2;

    let mut cmd = srvm(bin.path());
    cmd.env("PORT_FIXTURE_RELEASE", &release)
        .env("PORT_FIXTURE_HOLD", "1")
        .arg("--all")
        .arg("--port")
        .arg(start.to_string())
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    // Reservations are made before any child spawns, so the first child to say
    // it is waiting means every port is already chosen.
    wait_for(
        &out,
        &err,
        "fixture waiting before bind",
        Duration::from_secs(30),
    );
    // A just-released reservation can still hold its port exclusively during
    // socket teardown (Windows SO_EXCLUSIVEADDRUSE), so the steal retries
    // briefly instead of racing that close.
    let mut stolen = None;
    for _ in 0..25 {
        match TcpListener::bind(("127.0.0.1", npm_reserved)) {
            Ok(listener) => {
                stolen = Some(listener);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    let stolen = stolen.expect("steal the npm app's reserved port");
    fs::write(&release, "go").unwrap();

    let (seen, line) = wait_for_all_seen(
        &out,
        &err,
        &["app        [package:dev] http://127.0.0.1:"],
        Duration::from_secs(30),
    );
    let npm_port = app_line_port(&line[0]);

    assert!(
        seen.iter().any(|line| line.contains("retrying")),
        "the stolen port must trigger a handoff retry: {seen:?}"
    );
    assert!(
        npm_port > py_reserved,
        "the retry must clear the sibling's port {py_reserved}, got {npm_port}; srvm said:\n{}",
        seen.join("\n")
    );
    assert!(
        !seen
            .iter()
            .any(|line| line.contains("[django]") && line.contains("retrying")),
        "the sibling must not be disturbed: {seen:?}"
    );

    // Now the sibling binds the port it reserved all along.
    fs::write(&py_release, "go").unwrap();
    let line = wait_for(
        &out,
        &err,
        "app        [django] http://127.0.0.1:",
        Duration::from_secs(30),
    );
    let py_port = app_line_port(&line);

    assert_eq!(py_port, py_reserved, "the sibling keeps its own port");
    assert_ne!(npm_port, py_port);

    assert!(http_get(npm_port).starts_with("HTTP/1.1 200"));
    assert!(http_get(py_port).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
    drop(stolen);
}

/// A stub for `python3` whose fixture waits on its own release file, so a test
/// can hold one app back while another one fails and retries.
fn write_python_stub(dir: &Path, release: &Path) {
    #[cfg(windows)]
    {
        fs::write(
            dir.join("python3.cmd"),
            format!(
                "@echo off\r\nset PORT_FIXTURE_MODE=argv-port\r\nset PORT_FIXTURE_RELEASE={}\r\n\"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
                release.display(),
                fixture_bin().display()
            ),
        )
        .unwrap();
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("python3");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nPORT_FIXTURE_MODE=argv-port PORT_FIXTURE_HOLD=1 PORT_FIXTURE_RELEASE={} exec \"{}\" \"$@\"\n",
                release.display(),
                fixture_bin().display()
            ),
        )
        .unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }
}

/// A port-injected app that dies without ever announcing a URL has lost the
/// reservation-to-bind handoff, and that must be retried even when the death is
/// not quick: the thief is often gone by the time srvm looks, so an occupancy
/// probe cannot be the only evidence. Before this rule, a stolen port on a slow
/// start failed the whole launch.
#[test]
fn all_retries_a_slow_death_that_never_announced() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = package_only_repo();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("fail-once-slow"));
    let once = repo.path().join("once");

    let mut cmd = srvm(bin.path());
    cmd.env("PORT_FIXTURE_ONCE", &once)
        .arg("--all")
        .arg("--port")
        .arg("0")
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let (seen, lines) = wait_for_all_seen(
        &out,
        &err,
        &["app        http://127.0.0.1:"],
        Duration::from_secs(60),
    );
    let port = app_line_port(&lines[0]);

    assert!(
        seen.iter().any(|line| line.contains("retrying")),
        "a slow death without a URL must be retried: {seen:?}"
    );
    assert!(http_get(port).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}
