use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

use assert_cmd::Command;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[test]
fn path_scrubbed_fetch_boots_node_app() {
    let version = "v22.21.0";
    let target = srvm::runtime::node_target().unwrap();
    let filename = format!(
        "node-{version}-{target}.{}",
        srvm::runtime::node_archive_ext()
    );
    let (node_rel, npm_rel, node_body, npm_body) = fixture_node(version, target);
    let archive = archive(&[
        (&node_rel, node_body.as_bytes()),
        (&npm_rel, npm_body.as_bytes()),
    ]);
    let hex = hex_sha256(&archive);

    let mut routes = HashMap::new();
    routes.insert(
        "/index.json".into(),
        format!(r#"[{{"version":"{version}","lts":"Jod"}}]"#).into_bytes(),
    );
    routes.insert(
        format!("/{version}/SHASUMS256.txt"),
        format!("{hex}  {filename}\n").into_bytes(),
    );
    routes.insert(format!("/{version}/{filename}"), archive);
    let base = serve(routes);

    let repo = tempdir().unwrap();
    std::fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"dev":"node server.js"}}"#,
    )
    .unwrap();
    std::fs::write(repo.path().join("server.js"), "console.log('up')\n").unwrap();
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .arg("--dry-run")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("command    npm run dev"))
        .stdout(predicate::str::contains(
            "runtime    node (will fetch on launch)",
        ));
    assert!(cache.path().read_dir().unwrap().next().is_none());

    scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_NODE_INDEX_URL", format!("{base}/index.json"))
        .arg("--no-open")
        .arg("--no-install")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("fetching node {version}")))
        .stdout(predicate::str::contains("app        http://127.0.0.1:"));
}

#[test]
fn path_scrubbed_fetch_boots_python_app() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let triple = srvm::runtime::python_triple().unwrap();
    let version = "3.12.8";
    let (rel, body) = fixture_python();
    let archive = tar_gz(&[(&rel, body.as_bytes())]);
    let hex = hex_sha256(&archive);
    let filename = format!("cpython-{version}+20261004-{triple}-install_only.tar.gz");
    let mut routes = HashMap::new();
    routes.insert("/python.tar.gz".into(), archive);
    routes.insert(
        "/python.json".into(),
        format!(
            r#"{{"assets":[{{"name":"{filename}","browser_download_url":"{base}/python.tar.gz","digest":"sha256:{hex}"}}]}}"#
        )
        .into_bytes(),
    );
    serve_listener(listener, routes);

    let repo = tempdir().unwrap();
    std::fs::write(repo.path().join("manage.py"), "print('django')\n").unwrap();
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();

    scrubbed(path.path(), home.path(), cache.path())
        .env("SRVM_PYTHON_RELEASE_URL", format!("{base}/python.json"))
        .arg("--no-open")
        .arg("--no-install")
        .arg(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "fetching python {version}"
        )))
        .stdout(predicate::str::contains("app        http://127.0.0.1:"));
}

#[test]
fn path_scrubbed_fetch_boots_go_app() {
    let os = srvm::runtime::go_os().unwrap();
    let arch = srvm::runtime::go_arch().unwrap();
    let filename = format!("go1.22.10.{os}-{arch}.tar.gz");
    let rel = if cfg!(windows) {
        "go/bin/go.cmd"
    } else {
        "go/bin/go"
    };
    let body: &[u8] = if cfg!(windows) {
        b"@echo off\r\necho http://127.0.0.1:4321\r\n"
    } else {
        b"#!/bin/sh\necho http://127.0.0.1:${PORT:-4321}\n"
    };
    let archive = tar_gz(&[(rel, body)]);
    let hex = hex_sha256(&archive);
    let mut routes = HashMap::new();
    routes.insert(format!("/dl/{filename}"), archive);
    routes.insert(
        "/dl/".into(),
        format!(
            r#"[{{"version":"go1.22.10","stable":true,"files":[{{"filename":"{filename}","os":"{os}","arch":"{arch}","sha256":"{hex}","kind":"archive"}}]}}]"#
        )
        .into_bytes(),
    );
    let base = serve(routes);
    let repo = tempdir().unwrap();
    std::fs::write(
        repo.path().join("go.mod"),
        "module example.com/app\n\ngo 1.22.10\n",
    )
    .unwrap();
    std::fs::write(
        repo.path().join("main.go"),
        "package main\nfunc main() {}\n",
    )
    .unwrap();
    launch_scrubbed(
        &repo,
        &[("SRVM_GO_INDEX_URL", &format!("{base}/dl/?mode=json"))],
    )
    .stdout(predicate::str::contains("fetching go go1.22.10"))
    .stdout(predicate::str::contains("app        http://127.0.0.1:"));
}

#[test]
fn path_scrubbed_fetch_boots_rust_app() {
    let triple = srvm::runtime::rust_triple().unwrap();
    let version = "1.81.0";
    let (rustc_name, cargo_name, std_name) = (
        format!("rustc-{version}-{triple}.tar.gz"),
        format!("cargo-{version}-{triple}.tar.gz"),
        format!("rust-std-{version}-{triple}.tar.gz"),
    );
    let rustc_rel = format!("rustc-{version}-{triple}/rustc/bin/rustc");
    let cargo_rel = format!("cargo-{version}-{triple}/cargo/bin/cargo");
    let rustc = hashed_tool(&rustc_rel, "#!/bin/sh\nexit 0\n");
    let cargo = hashed_tool(
        &cargo_rel,
        "#!/bin/sh\necho http://127.0.0.1:${PORT:-4321}\n",
    );
    // rust-std ships as its own component and must merge into the toolchain
    // prefix; the fixture archive carries a single rlib under lib/rustlib.
    let std_rel = format!("rust-std-{version}-{triple}/lib/rustlib/{triple}/lib/libstd.rlib");
    let std_archive = tar_gz(&[(&std_rel, &b"stdlib"[..])]);
    let std_sums = format!(
        "{}  {}\n",
        hex_sha256(&std_archive),
        std_rel.rsplit('/').next().unwrap_or("libstd.rlib")
    );
    let mut routes = HashMap::new();
    routes.insert(
        "/dist/channel-rust-1.81.0.toml".into(),
        b"[pkg.rustc]\nversion = \"1.81.0 (fixture)\"\n".to_vec(),
    );
    routes.insert(format!("/dist/{rustc_name}"), rustc.0);
    routes.insert(format!("/dist/{rustc_name}.sha256"), rustc.1);
    routes.insert(format!("/dist/{cargo_name}"), cargo.0);
    routes.insert(format!("/dist/{cargo_name}.sha256"), cargo.1);
    routes.insert(format!("/dist/{std_name}"), std_archive);
    routes.insert(format!("/dist/{std_name}.sha256"), std_sums.into_bytes());
    let base = serve(routes);
    let repo = tempdir().unwrap();
    std::fs::create_dir(repo.path().join("src")).unwrap();
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        repo.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.81.0\"\n",
    )
    .unwrap();
    launch_scrubbed(&repo, &[("SRVM_RUST_DIST_URL", &format!("{base}/dist"))])
        .stdout(predicate::str::contains("fetching rust 1.81.0"))
        .stdout(predicate::str::contains("app        http://127.0.0.1:"));
}

fn hashed_tool(rel: &str, unix_body: &str) -> (Vec<u8>, Vec<u8>) {
    let (rel, body): (String, &[u8]) = if cfg!(windows) {
        (
            format!("{rel}.cmd"),
            b"@echo off\r\necho http://127.0.0.1:4321\r\n",
        )
    } else {
        (rel.to_string(), unix_body.as_bytes())
    };
    let archive = tar_gz(&[(&rel, body)]);
    let sums = format!(
        "{}  {}\n",
        hex_sha256(&archive),
        rel.rsplit('/').next().unwrap_or(&rel)
    );
    (archive, sums.into_bytes())
}

fn launch_scrubbed(repo: &tempfile::TempDir, extra: &[(&str, &str)]) -> assert_cmd::assert::Assert {
    let cache = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let mut cmd = scrubbed(path.path(), home.path(), cache.path());
    cmd.arg("--no-open").arg("--no-install").arg(repo.path());
    for (key, value) in extra {
        cmd.env(key, value);
    }
    cmd.assert().success()
}

/// srvm with PATH, HOME and the cache pointed at empty tempdirs. On Windows
/// the well-known-dir lookup (%ProgramFiles%\nodejs, %LOCALAPPDATA%\pnpm) is
/// scrubbed too, or a CI runner's real toolchain leaks in and nothing fetches.
fn scrubbed(path: &Path, home: &Path, cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("srvm").unwrap();
    cmd.env("PATH", path)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ProgramFiles", home)
        .env("ProgramFiles(x86)", home)
        .env("ProgramW6432", home)
        .env("LOCALAPPDATA", home)
        .env_remove("PORT")
        .env("SRVM_CACHE_DIR", cache);
    cmd
}

fn fixture_node(version: &str, target: &str) -> (String, String, String, String) {
    #[cfg(windows)]
    {
        (
            format!("node-{version}-{target}/node.cmd"),
            format!("node-{version}-{target}/npm.cmd"),
            "@echo off\r\necho ready on http://127.0.0.1:4321\r\n".into(),
            "@echo off\r\nnode\r\n".into(),
        )
    }
    #[cfg(not(windows))]
    {
        (
            format!("node-{version}-{target}/bin/node"),
            format!("node-{version}-{target}/bin/npm"),
            "#!/bin/sh\necho ready on http://127.0.0.1:${PORT:-4321}\n".into(),
            "#!/bin/sh\nexec \"$(/usr/bin/dirname \"$0\")/node\"\n".into(),
        )
    }
}

fn fixture_python() -> (String, String) {
    #[cfg(windows)]
    {
        (
            "python/python3.cmd".into(),
            "@echo off\r\necho http://127.0.0.1:%3\r\n".into(),
        )
    }
    #[cfg(not(windows))]
    {
        (
            "python/bin/python3".into(),
            "#!/bin/sh\necho http://127.0.0.1:${3:-8000}\n".into(),
        )
    }
}

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    #[cfg(windows)]
    {
        zip_files(files)
    }
    #[cfg(not(windows))]
    {
        tar_gz(files)
    }
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut raw = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut raw);
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, *body).unwrap();
        }
        builder.finish().unwrap();
    }
    let mut gz = Vec::new();
    let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();
    gz
}

#[cfg(windows)]
fn zip_files(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        for (path, body) in files {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serve(routes: HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    serve_listener(listener, routes);
    format!("http://127.0.0.1:{port}")
}

fn serve_listener(listener: TcpListener, routes: HashMap<String, Vec<u8>>) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() > 16384 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let path = req
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/");
            let body = routes.get(path).cloned().unwrap_or_default();
            let status = if routes.contains_key(path) {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
}
