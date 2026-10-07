//! Windows-only lifecycle regression: a real console control event must take
//! the whole app tree down through srvm's own teardown.
//!
//! The app and its descendant install console handlers that swallow the event
//! themselves, so their death proves srvm's tree teardown — not the console
//! default handler, and not this test's `taskkill` fallback, which only runs
//! when the `ChildGuard` is dropped after every assertion here.
#![cfg(windows)]

mod support;

use std::{
    fs,
    os::windows::process::CommandExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use support::{ChildGuard, SERIAL, app_line_port, line_reader, srvm, wait_exit, wait_for};
use tempfile::{TempDir, tempdir};

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CTRL_BREAK_EVENT: u32 = 1;
const STILL_ACTIVE: u32 = 259;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GenerateConsoleCtrlEvent(event: u32, process_group: u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn GetExitCodeProcess(process: isize, code: *mut u32) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn GetLastError() -> u32;
}

/// A repository with its dependencies already installed, so srvm launches the
/// `dev` script instead of bootstrapping. The stub IS the fixture binary
/// copied to `npm.exe`, so srvm resolves and spawns it directly: no `cmd.exe`
/// layer that the console event could kill independently of srvm's teardown.
fn app_repo() -> (TempDir, TempDir) {
    let repo = tempdir().unwrap();
    fs::create_dir(repo.path().join("node_modules")).unwrap();
    fs::write(
        repo.path().join("package.json"),
        r#"{"name":"app","scripts":{"dev":"node index.js"}}"#,
    )
    .unwrap();
    let bin = tempdir().unwrap();
    fs::copy(support::fixture_bin(), bin.path().join("npm.exe")).unwrap();
    (repo, bin)
}

fn read_pid(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "no pid recorded at {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            // A recycled pid can briefly look alive again; the generous
            // deadline below is the same trade the unix tests make.
            return false;
        }
        let mut code = STILL_ACTIVE;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

fn assert_gone(pid: u32, what: &str) {
    // Generous on purpose: this test runs alongside the rest of the suite.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("{what} (pid {pid}) survived teardown");
}

/// Deliver a real console control event to srvm's process group. The group was
/// created detached (CREATE_NEW_PROCESS_GROUP) but shares the test's console,
/// which is exactly the combination CTRL_BREAK_EVENT can target.
fn send_ctrl_break(group: u32) {
    unsafe {
        if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group) == 0 {
            panic!(
                "GenerateConsoleCtrlEvent failed (error {}): the test process \
                 needs a real console, so this regression must run on a \
                 console-attached runner, not a detached service",
                GetLastError()
            );
        }
    }
}

#[test]
fn a_real_console_break_event_takes_the_whole_tree_down() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (repo, bin) = app_repo();
    let app_pid_file = repo.path().join("app.pid");
    let descendant_pid_file = repo.path().join("descendant.pid");

    let mut cmd = srvm(bin.path());
    // srvm gets its own process group so the event can target srvm and its
    // tree without hitting the test runner's group.
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    cmd.arg(repo.path())
        .env("PORT_FIXTURE_MODE", "hold-break")
        .env("PORT_FIXTURE_PID_FILE", &app_pid_file)
        .env("PORT_FIXTURE_CHILD_PID_FILE", &descendant_pid_file);

    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    let url_line = wait_for(&out, &err, "http://127.0.0.1:", Duration::from_secs(30));
    let port = app_line_port(&url_line);
    let app = read_pid(&app_pid_file, Duration::from_secs(10));
    let descendant = read_pid(&descendant_pid_file, Duration::from_secs(10));

    // Both the app and the descendant swallow the console event themselves,
    // so this real CTRL_BREAK_EVENT can only stop them via srvm's teardown.
    send_ctrl_break(child.0.id());

    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert_eq!(
        status.code(),
        Some(130),
        "a console break must exit 130: {status}"
    );

    // Every assertion below runs before ChildGuard::drop — its taskkill
    // fallback is exactly what this test must prove unnecessary.
    assert_gone(app, "the app");
    assert_gone(descendant, "a console-event-resistant descendant");

    // The announced listener is actually released, not just the PIDs.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "announced port {port} never freed"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
