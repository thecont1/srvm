use std::{fs, path::Path, time::Duration};

use tempfile::{TempDir, tempdir};

mod support;
use support::*;

fn js_app(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();
}

fn django_app(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("manage.py"), "").unwrap();
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn bare_srvm_runs_every_conventional_app() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    django_app(&repo.path().join("backend"));
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "python3", Some("argv-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--port").arg("0").arg(repo.path());
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

/// App roots are reported with the host separator (`apps\web` on Windows), so
/// literal expectations have to be built the same way.
fn native(rel: &str) -> String {
    rel.replace('/', std::path::MAIN_SEPARATOR_STR)
}

#[test]
fn dry_run_lists_candidates_and_the_default_set_without_side_effects() {
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("apps/web"));
    js_app(&repo.path().join("apps/admin"));
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    let log = repo.path().join("fixture.log");

    let output = srvm(bin.path())
        .env("PORT_FIXTURE_LOG", &log)
        .arg("--dry-run")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("match      1. package:dev"), "{stdout}");
    assert!(
        stdout.contains(&format!("root       {}", native("apps/admin"))),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("root       {}", native("apps/web"))),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "launch     1. {}:package:dev  [{}]",
            native("apps/admin"),
            native("apps/admin")
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "launch     2. {}:package:dev  [{}]",
            native("apps/web"),
            native("apps/web")
        )),
        "{stdout}"
    );
    assert!(
        !repo
            .path()
            .join("apps/web/srvm-fixture-install.txt")
            .exists(),
        "dry-run must not install"
    );
    assert!(!log.exists(), "dry-run must not spawn anything");
}

#[test]
fn root_orchestrator_runs_alone_and_names_the_sub_apps() {
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    fs::write(repo.path().join("Makefile"), "dev:\n\tnpm run dev\n").unwrap();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    stub_exec_mode(bin.path(), "make", Some("env-port"));

    let output = srvm(bin.path())
        .arg("--dry-run")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("launch     1. make:dev  [.]"), "{stdout}");
    assert!(
        stdout.contains("orchestrated by make:dev; --select frontend:package:dev for one app"),
        "{stdout}"
    );
    assert!(
        stdout.contains("idle       frontend:package:dev (frontend)"),
        "{stdout}"
    );

    let selected = srvm(bin.path())
        .args(["--dry-run", "--select", "frontend:package:dev"])
        .arg(repo.path())
        .output()
        .unwrap();
    assert!(selected.status.success());
    let selected_stdout = stdout_of(&selected);
    assert!(
        selected_stdout.contains("launch     1. frontend:package:dev  [frontend]"),
        "{selected_stdout}"
    );
    assert!(
        !selected_stdout.contains("idle "),
        "a selection has nothing idling: {selected_stdout}"
    );
}

#[test]
fn qualified_select_runs_exactly_one_app() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("apps/web"));
    js_app(&repo.path().join("apps/admin"));
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--no-install")
        .arg("--select")
        .arg(format!("{}:package:dev", native("apps/admin")))
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let line = wait_for(&out, &err, "app        ", Duration::from_secs(30));
    assert!(
        !line.contains('['),
        "a single selected app stays unlabeled: {line}"
    );
    assert!(http_get(app_line_port(&line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn installs_run_in_each_app_root() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("apps/web"));
    js_app(&repo.path().join("apps/admin"));
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let mut cmd = srvm(bin.path());
    cmd.arg("--port").arg("0").arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let admin = format!(
        "app        [{}:package:dev] http://127.0.0.1:",
        native("apps/admin")
    );
    let web = format!(
        "app        [{}:package:dev] http://127.0.0.1:",
        native("apps/web")
    );
    let lines = wait_for_all(&out, &err, &[&admin, &web], Duration::from_secs(30));

    for path in [
        repo.path().join("apps/web/srvm-fixture-install.txt"),
        repo.path().join("apps/admin/srvm-fixture-install.txt"),
    ] {
        assert!(path.exists(), "install must run in {path:?}");
    }
    assert!(
        !repo.path().join("srvm-fixture-install.txt").exists(),
        "no install runs at the workspace root"
    );

    for line in &lines {
        assert!(http_get(app_line_port(line)).starts_with("HTTP/1.1 200"));
    }
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn paths_with_spaces_and_non_ascii_are_discovered() {
    let outer = tempdir().unwrap();
    let repo = outer.path().join("my app ünïcode");
    js_app(&repo.join("frontend"));
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let output = srvm(bin.path())
        .arg("--dry-run")
        .arg(&repo)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("match      1. package:dev"), "{stdout}");
    assert!(stdout.contains("root       frontend"), "{stdout}");
}

#[test]
fn empty_workspace_reports_no_app() {
    let repo: TempDir = tempdir().unwrap();
    let bin = tempdir().unwrap();

    let output = srvm(bin.path()).arg(repo.path()).output().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no servable app detected"), "{stderr}");
}

#[test]
fn dotenv_values_reach_the_app_child() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    fs::write(
        repo.path().join("frontend/.env"),
        "SRVM_TEST_ENV=from-dotenv # trailing\n",
    )
    .unwrap();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    let log = repo.path().join("frontend/fixture.log");

    let mut cmd = srvm(bin.path());
    cmd.env("PORT_FIXTURE_LOG", &log).arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let line = wait_for(&out, &err, "app        ", Duration::from_secs(30));
    let record = fs::read_to_string(&log).unwrap();
    assert!(record.contains("SRVM_TEST_ENV=from-dotenv"), "{record}");

    assert!(http_get(app_line_port(&line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn ambient_environment_beats_dotenv() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    fs::write(
        repo.path().join("frontend/.env"),
        "SRVM_TEST_ENV=from-dotenv\n",
    )
    .unwrap();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));
    let log = repo.path().join("frontend/fixture.log");

    let mut cmd = srvm(bin.path());
    cmd.env("PORT_FIXTURE_LOG", &log)
        .env("SRVM_TEST_ENV", "from-os")
        .arg(repo.path());
    let mut child = ChildGuard::new(&mut cmd);
    let out = line_reader(child.0.stdout.take().unwrap());
    let err = line_reader(child.0.stderr.take().unwrap());

    let line = wait_for(&out, &err, "app        ", Duration::from_secs(30));
    let record = fs::read_to_string(&log).unwrap();
    assert!(record.contains("SRVM_TEST_ENV=from-os"), "{record}");
    assert!(!record.contains("from-dotenv"), "{record}");

    assert!(http_get(app_line_port(&line)).starts_with("HTTP/1.1 200"));
    assert!(wait_exit(&mut child.0, Duration::from_secs(30)).success());
}

#[test]
fn dry_run_reports_dotenv_and_warns_without_failing() {
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    fs::write(repo.path().join("frontend/.env"), "GOOD=1\n1BAD=2\n").unwrap();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let output = srvm(bin.path())
        .arg("--dry-run")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("env        .env (1 vars)"), "{stdout}");
    assert!(stderr.contains("warn       [frontend] line 2"), "{stderr}");
}

#[test]
fn a_sample_env_without_a_dotenv_is_announced() {
    let repo = tempdir().unwrap();
    js_app(&repo.path().join("frontend"));
    fs::write(repo.path().join("frontend/.env.example"), "API_URL=\n").unwrap();
    let bin = tempdir().unwrap();
    stub_exec_mode(bin.path(), "npm", Some("env-port"));

    let output = srvm(bin.path())
        .arg("--dry-run")
        .arg(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("no .env; .env.example exists"), "{stdout}");
}
