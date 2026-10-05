use std::{
    fs,
    net::TcpListener,
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
