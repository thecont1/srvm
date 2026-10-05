use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{
    archive::{extract_verified, find_tool},
    go, hint, node, python, rust,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Node,
    Python,
    Go,
    Rust,
}

pub fn kind_for(tool: &str) -> Option<RuntimeKind> {
    match tool {
        "node" | "npm" | "npx" => Some(RuntimeKind::Node),
        "python" | "python3" => Some(RuntimeKind::Python),
        "go" => Some(RuntimeKind::Go),
        "cargo" | "rustc" => Some(RuntimeKind::Rust),
        _ => None,
    }
}

pub fn can_fetch(tool: &str) -> bool {
    kind_for(tool).is_some()
}

pub fn node_target() -> Result<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-x64"),
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("windows", "x86_64") => Ok("win-x64"),
        ("windows", "aarch64") => Ok("win-arm64"),
        (os, arch) => bail!("no official node build for {os}-{arch}"),
    }
}

pub fn node_archive_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

pub fn python_triple() -> Result<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        (os, arch) => bail!("no python-build-standalone build for {os}-{arch}"),
    }
}

const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

pub struct Endpoints {
    pub node_index_url: String,
    pub python_release_url: String,
    pub go_index_url: String,
    pub rust_dist_url: String,
}

impl Endpoints {
    pub fn from_env() -> Self {
        Self {
            node_index_url: env::var("SRVM_NODE_INDEX_URL")
                .unwrap_or_else(|_| "https://nodejs.org/dist/index.json".into()),
            python_release_url: env::var("SRVM_PYTHON_RELEASE_URL").unwrap_or_else(|_| {
                "https://api.github.com/repos/astral-sh/python-build-standalone/releases/latest"
                    .into()
            }),
            go_index_url: env::var("SRVM_GO_INDEX_URL")
                .unwrap_or_else(|_| "https://go.dev/dl/?mode=json".into()),
            rust_dist_url: env::var("SRVM_RUST_DIST_URL")
                .unwrap_or_else(|_| "https://static.rust-lang.org/dist".into()),
        }
    }
}

pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("SRVM_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    dirs::cache_dir()
        .map(|dir| dir.join("srvm"))
        .context("could not resolve a platform cache directory")
}

pub trait HttpGet {
    fn get(&self, url: &str) -> Result<Vec<u8>>;
}

pub struct UreqClient;

impl HttpGet for UreqClient {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        allow_url(url)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(180)))
            .https_only(url.starts_with("https://"))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(url)
            .header("User-Agent", "srvm/0.1.0")
            .call()
            .with_context(|| format!("GET {url}"))?;
        let mut limited = response.body_mut().as_reader().take(MAX_DOWNLOAD_BYTES + 1);
        let mut bytes = Vec::new();
        limited
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading {url}"))?;
        if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
            bail!("response from {url} exceeds {MAX_DOWNLOAD_BYTES} bytes");
        }
        Ok(bytes)
    }
}

pub fn fetch_if_missing(kind: RuntimeKind, root: &Path, quiet: bool) -> Result<Vec<PathBuf>> {
    ensure(
        kind,
        root,
        &cache_dir()?,
        &UreqClient,
        &Endpoints::from_env(),
        quiet,
    )
}

pub fn ensure(
    kind: RuntimeKind,
    root: &Path,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    match kind {
        RuntimeKind::Node => ensure_node(root, cache, client, endpoints, quiet),
        RuntimeKind::Python => ensure_python(root, cache, client, endpoints, quiet),
        RuntimeKind::Go => ensure_go(root, cache, client, endpoints, quiet),
        RuntimeKind::Rust => ensure_rust(root, cache, client, endpoints, quiet),
    }
}

fn ensure_node(
    root: &Path,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let index_url = &endpoints.node_index_url;
    let index = String::from_utf8(get(client, index_url)?).context("node index is not utf-8")?;
    let choice = node::select_node(
        &index,
        hint::node_want(root).as_ref(),
        node_target()?,
        node_archive_ext(),
    )?;
    let dest = cache.join("runtimes").join("node").join(&choice.version);
    if let Some(bins) = cached_bins(&dest, &[&["node"]]) {
        return Ok(bins);
    }
    if !quiet {
        println!("  step       fetching node {}", choice.version);
    }
    let root_url = dist_root(index_url)?;
    let shasums_url = format!("{root_url}/{}/SHASUMS256.txt", choice.version);
    let archive_url = format!("{root_url}/{}/{}", choice.version, choice.filename);
    let shasums = String::from_utf8(get(client, &shasums_url)?)
        .context("node SHASUMS256.txt is not utf-8")?;
    let expected = node::sha256_for(&shasums, &choice.filename)?;
    let bytes = get(client, &archive_url)?;
    install_archives(&[(&bytes, &expected)], &dest, &[&["node"]])
}

fn ensure_python(
    root: &Path,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let release = String::from_utf8(get(client, &endpoints.python_release_url)?)
        .context("python release JSON is not utf-8")?;
    let choice = python::select_python(
        &release,
        hint::python_want(root).as_deref(),
        python_triple()?,
    )?;
    let dest = cache.join("runtimes").join("python").join(&choice.version);
    if let Some(bins) = cached_bins(&dest, &[&["python3", "python"]]) {
        return Ok(bins);
    }
    if !quiet {
        println!("  step       fetching python {}", choice.version);
    }
    allow_url(&choice.url)?;
    let bytes = get(client, &choice.url)?;
    install_archives(
        &[(&bytes, &choice.sha256)],
        &dest,
        &[&["python3", "python"]],
    )
}

fn ensure_go(
    root: &Path,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let index_url = &endpoints.go_index_url;
    let index =
        String::from_utf8(get(client, index_url)?).context("go release index is not utf-8")?;
    let choice = go::select_go(&index, hint::go_want(root).as_ref(), go_os()?, go_arch()?)?;
    let dest = cache.join("runtimes").join("go").join(&choice.version);
    if let Some(bins) = cached_bins(&dest, &[&["go"]]) {
        return Ok(bins);
    }
    if !quiet {
        println!("  step       fetching go {}", choice.version);
    }
    let archive_url = format!("{}/{}", url_dir(index_url)?, choice.filename);
    let bytes = get(client, &archive_url)?;
    install_archives(&[(&bytes, &choice.sha256)], &dest, &[&["go"]])
}

fn ensure_rust(
    root: &Path,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let dist = endpoints.rust_dist_url.trim_end_matches('/');
    let channel = rust::channel_filename(hint::rust_channel(root).as_deref())?;
    let manifest = String::from_utf8(get(client, &format!("{dist}/{channel}"))?)
        .context("rust channel manifest is not utf-8")?;
    let version = rust::version_from_channel_toml(&manifest)?;
    let dest = cache.join("runtimes").join("rust").join(&version);
    if let Some(bins) = cached_bins(&dest, &[&["cargo"], &["rustc"]]) {
        return Ok(bins);
    }
    if !quiet {
        println!("  step       fetching rust {version}");
    }
    let triple = rust_triple()?;
    let (rustc_name, cargo_name) = rust::component_filenames(&version, triple);
    let rustc = fetch_hashed(client, dist, &rustc_name)?;
    let cargo = fetch_hashed(client, dist, &cargo_name)?;
    install_archives(
        &[(&rustc.0, &rustc.1), (&cargo.0, &cargo.1)],
        &dest,
        &[&["cargo"], &["rustc"]],
    )
}

fn fetch_hashed(client: &dyn HttpGet, dist: &str, filename: &str) -> Result<(Vec<u8>, [u8; 32])> {
    let bytes = get(client, &format!("{dist}/{filename}"))?;
    let sums = String::from_utf8(get(client, &format!("{dist}/{filename}.sha256"))?)
        .with_context(|| format!("{filename}.sha256 is not utf-8"))?;
    let expected = node::sha256_for(&sums, filename).or_else(|_| {
        let hex = sums.split_whitespace().next().unwrap_or("");
        node::decode_sha256(hex)
    })?;
    Ok((bytes, expected))
}

pub fn go_os() -> Result<&'static str> {
    match env::consts::OS {
        "macos" => Ok("darwin"),
        "linux" => Ok("linux"),
        "windows" => Ok("windows"),
        os => bail!("no official go build for {os}"),
    }
}

pub fn go_arch() -> Result<&'static str> {
    match env::consts::ARCH {
        "aarch64" => Ok("arm64"),
        "x86_64" => Ok("amd64"),
        arch => bail!("no official go build for {arch}"),
    }
}

pub fn rust_triple() -> Result<&'static str> {
    python_triple().context("no rust build for this platform")
}

fn get(client: &dyn HttpGet, url: &str) -> Result<Vec<u8>> {
    allow_url(url)?;
    client.get(url).with_context(|| format!("GET {url}"))
}

pub fn allow_url(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if host == "127.0.0.1" || host == "localhost" {
            return Ok(());
        }
    }
    bail!("refusing non-https URL {url}")
}

fn dist_root(index_url: &str) -> Result<String> {
    index_url
        .trim_end_matches('/')
        .strip_suffix("/index.json")
        .map(str::to_string)
        .context("node index URL must end with /index.json")
}

fn url_dir(url: &str) -> Result<String> {
    let path = url.split('?').next().unwrap_or(url).trim_end_matches('/');
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        Ok(path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| path.to_string()))
    } else {
        Ok(path.to_string())
    }
}

fn cached_bins(dest: &Path, groups: &[&[&str]]) -> Option<Vec<PathBuf>> {
    if !dest.join(".srvm-ok").is_file() {
        return None;
    }
    let mut dirs = Vec::new();
    for names in groups {
        let bin = find_tool(dest, names)?.parent()?.to_path_buf();
        if !dirs.contains(&bin) {
            dirs.push(bin);
        }
    }
    Some(dirs)
}

fn install_archives(
    archives: &[(&[u8], &[u8; 32])],
    dest: &Path,
    groups: &[&[&str]],
) -> Result<Vec<PathBuf>> {
    let partial = dest.with_file_name(format!(
        "{}.partial",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    if partial.exists() {
        fs::remove_dir_all(&partial)?;
    }
    let installed: Result<()> = (|| {
        fs::create_dir_all(&partial)?;
        for (bytes, expected) in archives {
            extract_verified(bytes, expected, &partial)?;
        }
        fs::write(partial.join(".srvm-ok"), b"ok")?;
        if dest.exists() {
            fs::remove_dir_all(dest)?;
        }
        fs::rename(&partial, dest)?;
        Ok(())
    })();
    if installed.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    installed?;
    cached_bins(dest, groups)
        .with_context(|| "archive did not contain the expected toolchain binaries")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, io::Write, sync::Mutex};

    use sha2::{Digest, Sha256};

    struct MapClient {
        files: HashMap<String, Vec<u8>>,
        hits: Mutex<Vec<String>>,
    }

    impl HttpGet for MapClient {
        fn get(&self, url: &str) -> Result<Vec<u8>> {
            self.hits.lock().unwrap().push(url.to_string());
            self.files
                .get(url)
                .cloned()
                .with_context(|| format!("missing fixture {url}"))
        }
    }

    #[test]
    fn refuses_non_loopback_http() {
        let err = allow_url("http://evil.example/node.tar.gz").unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert!(allow_url("https://nodejs.org/dist/index.json").is_ok());
        assert!(allow_url("http://127.0.0.1:9/index.json").is_ok());
    }

    #[test]
    fn node_install_verifies_and_reuses_cache() {
        let version = "v22.21.0";
        let target = node_target().unwrap();
        let filename = format!("node-{version}-{target}.{}", node_archive_ext());
        let tool_rel = if cfg!(windows) {
            format!("node-{version}-{target}/node.exe")
        } else {
            format!("node-{version}-{target}/bin/node")
        };
        let archive = host_archive(&[(&tool_rel, b"#!/bin/sh\necho node\n")]);
        let digest = Sha256::digest(&archive);
        let hex = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let base = "http://127.0.0.1:9";
        let mut files = HashMap::new();
        files.insert(
            format!("{base}/index.json"),
            format!(r#"[{{"version":"{version}","lts":"Jod"}}]"#).into_bytes(),
        );
        files.insert(
            format!("{base}/{version}/SHASUMS256.txt"),
            format!("{hex}  {filename}\n").into_bytes(),
        );
        files.insert(format!("{base}/{version}/{filename}"), archive);
        let client = MapClient {
            files,
            hits: Mutex::new(Vec::new()),
        };
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let endpoints = Endpoints {
            node_index_url: format!("{base}/index.json"),
            python_release_url: format!("{base}/python.json"),
            go_index_url: format!("{base}/go.json"),
            rust_dist_url: base.into(),
        };
        let bins = ensure(
            RuntimeKind::Node,
            root.path(),
            cache.path(),
            &client,
            &endpoints,
            true,
        )
        .unwrap();
        let bin = &bins[0];
        assert!(
            bin.join(if cfg!(windows) { "node.exe" } else { "node" })
                .is_file()
        );

        ensure(
            RuntimeKind::Node,
            root.path(),
            cache.path(),
            &client,
            &endpoints,
            true,
        )
        .unwrap();
        let hits = client.hits.lock().unwrap();
        assert_eq!(
            hits.iter().filter(|url| url.ends_with(&filename)).count(),
            1
        );
    }

    #[test]
    fn checksum_mismatch_does_not_leave_a_runtime() {
        let version = "v22.21.0";
        let target = node_target().unwrap();
        let filename = format!("node-{version}-{target}.{}", node_archive_ext());
        let tool_rel = if cfg!(windows) {
            format!("node-{version}-{target}/node.exe")
        } else {
            format!("node-{version}-{target}/bin/node")
        };
        let archive = host_archive(&[(&tool_rel, b"#!/bin/sh\necho node\n")]);
        let base = "http://127.0.0.1:9";
        let mut files = HashMap::new();
        files.insert(
            format!("{base}/index.json"),
            format!(r#"[{{"version":"{version}","lts":"Jod"}}]"#).into_bytes(),
        );
        files.insert(
            format!("{base}/{version}/SHASUMS256.txt"),
            format!(
                "{}  {filename}\n",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .into_bytes(),
        );
        files.insert(format!("{base}/{version}/{filename}"), archive);
        let client = MapClient {
            files,
            hits: Mutex::new(Vec::new()),
        };
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let endpoints = Endpoints {
            node_index_url: format!("{base}/index.json"),
            python_release_url: format!("{base}/python.json"),
            go_index_url: format!("{base}/go.json"),
            rust_dist_url: base.into(),
        };
        let err = ensure(
            RuntimeKind::Node,
            root.path(),
            cache.path(),
            &client,
            &endpoints,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(
            !cache
                .path()
                .join("runtimes/node")
                .join(version)
                .join(".srvm-ok")
                .exists()
        );
    }

    fn host_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        #[cfg(windows)]
        {
            zip_files(files)
        }
        #[cfg(not(windows))]
        {
            tar_gz(files)
        }
    }

    #[cfg(not(windows))]
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
}
