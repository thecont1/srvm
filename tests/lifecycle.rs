//! Lifecycle regressions: what a launcher owes the user when it is told to
//! stop, and what it must never leave running.
//!
//! Unix only for now. The Windows console-event regression needs a real
//! `CTRL_C_EVENT` and lands with that sub-gate; until then Windows teardown is
//! covered by the multi-app tests, whose guard makes them weaker evidence.
#![cfg(unix)]

mod support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use support::{ChildGuard, SERIAL, line_reader, srvm, wait_exit, wait_for};
use tempfile::{TempDir, tempdir};

/// A repository whose `dev` script srvm will run through the stub `npm` on
/// PATH. Dependencies are present, so srvm spawns the script instead of
/// installing first.
fn app_repo() -> TempDir {
    let repo = app_repo_without_deps();
    fs::create_dir(repo.path().join("node_modules")).unwrap();
    repo
}

/// The same repository with no installed dependencies, so srvm bootstraps.
fn app_repo_without_deps() -> TempDir {
    let repo = tempdir().unwrap();
    fs::write(
        repo.path().join("package.json"),
        r#"{"name":"app","scripts":{"dev":"node index.js"}}"#,
    )
    .unwrap();
    repo
}

/// The real fixture listener, so an announced URL is actually served and srvm
/// treats the app as running instead of failing the launch.
fn listener() -> String {
    support::fixture_bin().display().to_string()
}

/// Writes an executable stub. Its body decides what srvm actually gets.
fn stub(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn read_pid(path: &Path, timeout: Duration) -> i32 {
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

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn assert_gone(pid: i32, what: &str) {
    // Generous on purpose: these tests run alongside the rest of the suite, and
    // a loaded machine must not turn a slow teardown into a failure.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("{what} (pid {pid}) survived teardown");
}

fn interrupt(child: &mut ChildGuard) {
    unsafe {
        libc::kill(child.0.id() as libc::pid_t, libc::SIGINT);
    }
}

#[test]
fn a_term_resistant_descendant_does_not_survive_ctrl_c() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = app_repo();
    let bin = tempdir().unwrap();
    let pid_file = repo.path().join("descendant.pid");
    // The app spawns a descendant that traps TERM: asking politely is not
    // enough, so teardown has to escalate.
    stub(
        bin.path(),
        "npm",
        &format!(
            "#!/bin/sh\n\
             /bin/sh -c 'trap \"\" TERM; echo $$ > {pid}; while :; do /bin/sleep 1; done' &\n\
             exec \"{listener}\" \"$@\"\n",
            pid = pid_file.display(),
            listener = listener()
        ),
    );

    let mut cmd = srvm(bin.path());
    cmd.arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    wait_for(&out, &err, "http://127.0.0.1:", Duration::from_secs(30));
    let descendant = read_pid(&pid_file, Duration::from_secs(10));

    interrupt(&mut child);
    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert_eq!(status.code(), Some(130), "SIGINT must exit 130: {status}");
    assert_gone(descendant, "a TERM-resistant descendant");
}

#[test]
fn a_grandchild_holding_the_pipes_does_not_block_shutdown() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = app_repo();
    let bin = tempdir().unwrap();
    let pid_file = repo.path().join("grandchild.pid");
    // The grandchild inherits srvm's pipes and outlives the request to stop,
    // so a shutdown that joins the pumps unboundedly would hang here.
    stub(
        bin.path(),
        "npm",
        &format!(
            "#!/bin/sh\n\
             /bin/sh -c 'echo $$ > {pid}; /bin/sleep 300' &\n\
             exec \"{listener}\" \"$@\"\n",
            pid = pid_file.display(),
            listener = listener()
        ),
    );

    let mut cmd = srvm(bin.path());
    cmd.arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());
    wait_for(&out, &err, "http://127.0.0.1:", Duration::from_secs(30));
    let grandchild = read_pid(&pid_file, Duration::from_secs(10));

    interrupt(&mut child);
    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert_eq!(status.code(), Some(130), "SIGINT must exit 130: {status}");
    assert_gone(grandchild, "a grandchild holding the pipes");
}

#[test]
fn ctrl_c_in_the_spawn_window_still_stops_the_app() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = app_repo();
    let bin = tempdir().unwrap();
    let pid_file = repo.path().join("app.pid");
    // The app starts but never announces, so the interrupt lands between the
    // spawn and the URL: the registration window teardown must still cover.
    stub(
        bin.path(),
        "npm",
        &format!(
            "#!/bin/sh\necho $$ > {pid}\n/bin/sleep 300\n",
            pid = pid_file.display()
        ),
    );

    let mut cmd = srvm(bin.path());
    cmd.arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let _out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());
    let app = read_pid(&pid_file, Duration::from_secs(30));

    interrupt(&mut child);
    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert_eq!(status.code(), Some(130), "SIGINT must exit 130: {status}");
    assert_gone(app, "an app stopped in its spawn window");
}

#[test]
fn ctrl_c_during_a_bootstrap_install_stops_the_install() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = app_repo_without_deps();
    let bin = tempdir().unwrap();
    let pid_file = repo.path().join("install.pid");
    // No node_modules in the repo, so srvm installs first: the interrupt has to
    // stop the install, not leave it running behind a launcher that exited.
    stub(
        bin.path(),
        "npm",
        &format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             install|ci)\n\
             \x20 echo $$ > {pid}\n\
             \x20 /bin/sleep 300\n\
             \x20 ;;\n\
             *)\n\
             \x20 exec \"{listener}\" \"$@\"\n\
             \x20 ;;\n\
             esac\n",
            pid = pid_file.display(),
            listener = listener()
        ),
    );

    let mut cmd = srvm(bin.path());
    cmd.arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let _out = line_reader(child.0.stdout.take().unwrap());
    let _err = line_reader(child.0.stderr.take().unwrap());
    let install = read_pid(&pid_file, Duration::from_secs(30));

    interrupt(&mut child);
    let status = wait_exit(&mut child.0, Duration::from_secs(15));
    assert_eq!(status.code(), Some(130), "SIGINT must exit 130: {status}");
    assert_gone(install, "a bootstrap install");
}
