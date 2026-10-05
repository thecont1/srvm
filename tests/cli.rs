use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn dry_run_detects_package_script() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_tool(bin.path(), "npm");
    fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"vite --host"}}"#,
    )
    .unwrap();

    srvm(bin.path())
        .arg("--dry-run")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("match      1. package:dev"))
        .stdout(predicate::str::contains("command    npm run dev"));
}

#[test]
fn dry_run_reports_no_detection() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();

    srvm(bin.path())
        .arg("--dry-run")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "detect     no servable app detected",
        ));
}

#[test]
fn supervisor_adopts_sniffed_url() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_tool_with(
        bin.path(),
        "npm",
        "echo ready on http://0.0.0.0:4321\nexit 0\n",
    );
    fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();

    srvm(bin.path())
        .arg("--no-open")
        .arg("--no-install")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app        http://127.0.0.1:4321"));
}

#[test]
fn supervisor_reports_early_failure_tail() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_tool_with(bin.path(), "npm", "echo boom >&2\nexit 7\n");
    fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();

    srvm(bin.path())
        .arg("--no-open")
        .arg("--no-install")
        .arg(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("package:dev failed early"))
        .stderr(predicate::str::contains("boom"));
}

#[test]
fn dry_run_rejects_invalid_inherited_port() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();
    stub_tool(bin.path(), "npm");
    fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();

    srvm(bin.path())
        .env("PORT", "not-a-port")
        .arg("--dry-run")
        .arg(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a valid port"));

    srvm(bin.path())
        .env("PORT", "not-a-port")
        .arg("--dry-run")
        .arg("--port")
        .arg("0")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OS-assigned"));
}

#[test]
fn all_flag_is_reserved() {
    let repo = tempdir().unwrap();
    let bin = tempdir().unwrap();

    srvm(bin.path())
        .arg("--all")
        .arg(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--all is reserved"));
}

fn srvm(bin: &Path) -> Command {
    let mut cmd = Command::cargo_bin("srvm").unwrap();
    cmd.env("PATH", bin);
    cmd
}

fn stub_tool(dir: &Path, name: &str) {
    stub_tool_with(dir, name, "exit 0\n");
}

fn stub_tool_with(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);

    #[cfg(windows)]
    {
        let body = body.replace("\n", "\r\n");
        fs::write(path.with_extension("cmd"), format!("@echo off\r\n{body}")).unwrap();
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }
}
