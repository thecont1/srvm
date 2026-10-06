use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{
    archive::{extract_verified, extract_verified_flat, find_tool},
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

pub fn fetch_if_missing(
    kind: RuntimeKind,
    scope: &hint::Scope,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    ensure(
        kind,
        scope,
        &cache_dir()?,
        &UreqClient,
        &Endpoints::from_env(),
        quiet,
    )
}

pub fn ensure(
    kind: RuntimeKind,
    scope: &hint::Scope,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    match kind {
        RuntimeKind::Node => ensure_node(scope, cache, client, endpoints, quiet),
        RuntimeKind::Python => ensure_python(scope, cache, client, endpoints, quiet),
        RuntimeKind::Go => ensure_go(scope, cache, client, endpoints, quiet),
        RuntimeKind::Rust => ensure_rust(scope, cache, client, endpoints, quiet),
    }
}

fn ensure_node(
    scope: &hint::Scope,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let index_url = &endpoints.node_index_url;
    let index = match get(client, index_url) {
        Ok(bytes) => String::from_utf8(bytes).context("node index is not utf-8")?,
        Err(err) => {
            return cached_fallback(
                cache,
                "node",
                &[&["node"]],
                |version| node_matches_cached(version, hint::node_want_in(scope).as_ref()),
                err,
            );
        }
    };
    let choice = node::select_node(
        &index,
        hint::node_want_in(scope).as_ref(),
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
    scope: &hint::Scope,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let release = match get(client, &endpoints.python_release_url) {
        Ok(bytes) => String::from_utf8(bytes).context("python release JSON is not utf-8")?,
        Err(err) => {
            return cached_fallback(
                cache,
                "python",
                &[&["python3", "python"]],
                |version| python_matches_cached(version, hint::python_want_in(scope).as_deref()),
                err,
            );
        }
    };
    let choice = python::select_python(
        &release,
        hint::python_want_in(scope).as_deref(),
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
    scope: &hint::Scope,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let index_url = &endpoints.go_index_url;
    let index = match get(client, index_url) {
        Ok(bytes) => String::from_utf8(bytes).context("go release index is not utf-8")?,
        Err(err) => {
            return cached_fallback(
                cache,
                "go",
                &[&["go"]],
                |version| go_matches_cached(version, hint::go_want_in(scope).as_ref()),
                err,
            );
        }
    };
    let choice = go::select_go(
        &index,
        hint::go_want_in(scope).as_ref(),
        go_os()?,
        go_arch()?,
    )?;
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
    scope: &hint::Scope,
    cache: &Path,
    client: &dyn HttpGet,
    endpoints: &Endpoints,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let dist = endpoints.rust_dist_url.trim_end_matches('/');
    let wanted = hint::rust_channel_in(scope);
    let channel = rust::channel_filename(wanted.as_deref())?;
    let manifest = match get(client, &format!("{dist}/{channel}")) {
        Ok(bytes) => String::from_utf8(bytes).context("rust channel manifest is not utf-8")?,
        Err(err) => {
            return cached_fallback(
                cache,
                "rust",
                &[&["cargo"], &["rustc"]],
                |version| rust_matches_cached(version, wanted.as_deref()),
                err,
            );
        }
    };
    let version = rust::version_from_channel_toml(&manifest)?;
    let dest = cache.join("runtimes").join("rust").join(&version);
    if let Some(bins) = cached_bins(&dest, &[&["cargo"], &["rustc"]]) {
        return Ok(bins);
    }
    if !quiet {
        println!("  step       fetching rust {version}");
    }
    let triple = rust_triple()?;
    let (rustc_name, cargo_name, std_name) = rust::component_filenames(&version, triple);
    let rustc = fetch_hashed(client, dist, &rustc_name)?;
    let cargo = fetch_hashed(client, dist, &cargo_name)?;
    let std = fetch_hashed(client, dist, &std_name)?;
    // rustc, cargo and rust-std ship as separate component archives whose
    // contents must share one prefix — rustc locates the standard library in
    // lib/rustlib next to its own binary, so a plain extraction that keeps
    // each component under its own root would leave std unfindable.
    install_archives_flat(
        &[(&rustc.0, &rustc.1), (&cargo.0, &cargo.1), (&std.0, &std.1)],
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

/// Fallback for a failed index/channel fetch: reuse the newest cached runtime
/// that matches the project's version hint, so offline launches keep working.
/// When nothing cached matches, the original network error is surfaced.
fn cached_fallback(
    cache: &Path,
    kind: &str,
    groups: &[&[&str]],
    matches: impl Fn(&str) -> bool,
    index_error: anyhow::Error,
) -> Result<Vec<PathBuf>> {
    let dir = cache.join("runtimes").join(kind);
    let mut candidates = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches(&name) {
                candidates.push(name);
            }
        }
    }
    candidates.sort_by(|left, right| cmp_versions(right, left));
    for version in candidates {
        if let Some(bins) = cached_bins(&dir.join(&version), groups) {
            return Ok(bins);
        }
    }
    Err(index_error)
}

fn node_matches_cached(version: &str, want: Option<&hint::NodeWant>) -> bool {
    match want {
        None | Some(hint::NodeWant::Lts(_)) | Some(hint::NodeWant::Latest) => true,
        Some(hint::NodeWant::Exact(wanted)) => version == wanted,
        Some(hint::NodeWant::Prefix(prefix)) => dotted_prefix(
            version.trim_start_matches('v'),
            prefix.trim_start_matches('v'),
        ),
    }
}

fn python_matches_cached(version: &str, want: Option<&str>) -> bool {
    match want {
        None => true,
        Some(prefix) => dotted_prefix(version, prefix),
    }
}

fn go_matches_cached(version: &str, want: Option<&hint::GoWant>) -> bool {
    let version = version.trim_start_matches("go");
    match want {
        None => true,
        Some(hint::GoWant::Exact(wanted)) => version == wanted,
        Some(hint::GoWant::Prefix(prefix)) => dotted_prefix(version, prefix),
    }
}

fn rust_matches_cached(version: &str, channel: Option<&str>) -> bool {
    match channel {
        None | Some("stable") => true,
        Some(channel) => dotted_prefix(version, channel),
    }
}

fn dotted_prefix(version: &str, prefix: &str) -> bool {
    version == prefix
        || version
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn cmp_versions(left: &str, right: &str) -> std::cmp::Ordering {
    match version_tuple(left).cmp(&version_tuple(right)) {
        std::cmp::Ordering::Equal => match (left.contains('-'), right.contains('-')) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        },
        other => other,
    }
}

fn version_tuple(version: &str) -> (u32, u32, u32) {
    let parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(0))
        .collect::<Vec<_>>();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

fn install_archives(
    archives: &[(&[u8], &[u8; 32])],
    dest: &Path,
    groups: &[&[&str]],
) -> Result<Vec<PathBuf>> {
    install_archives_mode(archives, dest, groups, false)
}

/// Variant for component-based toolchains (Rust): each archive's top-level
/// directory is merged into the shared toolchain prefix.
fn install_archives_flat(
    archives: &[(&[u8], &[u8; 32])],
    dest: &Path,
    groups: &[&[&str]],
) -> Result<Vec<PathBuf>> {
    install_archives_mode(archives, dest, groups, true)
}

fn install_archives_mode(
    archives: &[(&[u8], &[u8; 32])],
    dest: &Path,
    groups: &[&[&str]],
    flat: bool,
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
            if flat {
                extract_verified_flat(bytes, expected, &partial)?;
            } else {
                extract_verified(bytes, expected, &partial)?;
            }
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
            &hint::Scope::app(root.path()),
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
            &hint::Scope::app(root.path()),
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
            &hint::Scope::app(root.path()),
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
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                // `set_path` rejects a path over 100 bytes, and real component
                // archives nest deeply; let the builder emit a long-name entry.
                builder.append_data(&mut header, path, *body).unwrap();
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

    fn seed_cached(cache: &Path, kind: &str, version: &str, tools: &[&str]) {
        let dir = cache.join("runtimes").join(kind).join(version);
        fs::create_dir_all(dir.join("bin")).unwrap();
        for tool in tools {
            let tool = dir.join("bin").join(tool);
            fs::write(&tool, b"stub").unwrap();
            // Extraction preserves mode 0755; make the seeded stub executable
            // so find_tool accepts it like a real install would.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tool, fs::Permissions::from_mode(0o755));
            }
        }
        fs::write(dir.join(".srvm-ok"), b"ok").unwrap();
    }

    fn offline_client() -> MapClient {
        MapClient {
            files: HashMap::new(),
            hits: Mutex::new(Vec::new()),
        }
    }

    fn endpoints() -> Endpoints {
        Endpoints {
            node_index_url: "https://nodejs.org/dist/index.json".into(),
            python_release_url:
                "https://api.github.com/repos/astral-sh/python-build-standalone/releases/latest"
                    .into(),
            go_index_url: "https://go.dev/dl/?mode=json".into(),
            rust_dist_url: "https://static.rust-lang.org/dist".into(),
        }
    }

    #[test]
    fn offline_fallback_reuses_newest_hint_matching_cached_node() {
        let tool = if cfg!(windows) { "node.exe" } else { "node" };
        let cache = tempfile::tempdir().unwrap();
        seed_cached(cache.path(), "node", "v20.18.1", &[tool]);
        seed_cached(cache.path(), "node", "v22.21.0", &[tool]);
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".nvmrc"), b"22\n").unwrap();
        let client = offline_client();

        let bins = ensure(
            RuntimeKind::Node,
            &hint::Scope::app(root.path()),
            cache.path(),
            &client,
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(bins[0].join(tool).is_file());
        assert!(
            bins[0].to_string_lossy().contains("v22.21.0"),
            "picked {bins:?} instead of the newest 22.x"
        );
        let hits = client.hits.lock().unwrap();
        assert_eq!(
            hits.len(),
            1,
            "offline fallback must not download anything: {hits:?}"
        );
    }

    #[test]
    fn offline_fallback_prefers_the_hinted_version() {
        let tool = if cfg!(windows) { "node.exe" } else { "node" };
        let cache = tempfile::tempdir().unwrap();
        seed_cached(cache.path(), "node", "v20.18.1", &[tool]);
        seed_cached(cache.path(), "node", "v22.21.0", &[tool]);
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".nvmrc"), b"20\n").unwrap();

        let bins = ensure(
            RuntimeKind::Node,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(
            bins[0].to_string_lossy().contains("v20.18.1"),
            "picked {bins:?} instead of the hinted 20.x"
        );
    }

    #[test]
    fn offline_fallback_surfaces_the_index_error_when_nothing_cached() {
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let err = ensure(
            RuntimeKind::Node,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("GET https://nodejs.org/dist/index.json"),
            "expected the original index error, got: {err}"
        );
    }

    #[test]
    fn offline_fallback_covers_go_python_and_rust() {
        let cache = tempfile::tempdir().unwrap();
        seed_cached(
            cache.path(),
            "go",
            "go1.23.2",
            &[if cfg!(windows) { "go.exe" } else { "go" }],
        );
        seed_cached(
            cache.path(),
            "python",
            "3.12.7",
            &[if cfg!(windows) {
                "python.exe"
            } else {
                "python3"
            }],
        );
        seed_cached(
            cache.path(),
            "rust",
            "1.85.0",
            &[
                if cfg!(windows) { "cargo.exe" } else { "cargo" },
                if cfg!(windows) { "rustc.exe" } else { "rustc" },
            ],
        );
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("go.mod"), b"module x\n\ngo 1.23\n").unwrap();
        fs::write(root.path().join(".python-version"), b"3.12\n").unwrap();
        fs::write(
            root.path().join("rust-toolchain.toml"),
            b"[toolchain]\nchannel = \"stable\"\n",
        )
        .unwrap();

        let go_bins = ensure(
            RuntimeKind::Go,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(
            go_bins[0].to_string_lossy().contains("go1.23.2"),
            "{go_bins:?}"
        );

        let py_bins = ensure(
            RuntimeKind::Python,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(
            py_bins[0].to_string_lossy().contains("3.12.7"),
            "{py_bins:?}"
        );

        let rust_bins = ensure(
            RuntimeKind::Rust,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(
            rust_bins
                .iter()
                .all(|bin| bin.to_string_lossy().contains("1.85.0")),
            "{rust_bins:?}"
        );
    }

    #[test]
    fn offline_fallback_skips_cache_dirs_without_verified_marker() {
        let tool = if cfg!(windows) { "node.exe" } else { "node" };
        let cache = tempfile::tempdir().unwrap();
        // A partial/incomplete install: binaries but no .srvm-ok marker.
        let partial = cache.path().join("runtimes").join("node").join("v22.21.0");
        fs::create_dir_all(partial.join("bin")).unwrap();
        fs::write(partial.join("bin").join(tool), b"stub").unwrap();
        seed_cached(cache.path(), "node", "v20.18.1", &[tool]);
        let root = tempfile::tempdir().unwrap();

        let bins = ensure(
            RuntimeKind::Node,
            &hint::Scope::app(root.path()),
            cache.path(),
            &offline_client(),
            &endpoints(),
            true,
        )
        .unwrap();
        assert!(
            bins[0].to_string_lossy().contains("v20.18.1"),
            "an unverified cache dir must not satisfy the fallback: {bins:?}"
        );
    }

    fn hex_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn archive_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_slice()))
            .collect();
        host_archive(&refs)
    }

    /// The three archives a real Rust release ships, named and nested the way
    /// the dist server nests them: each tarball wraps a payload directory
    /// inside a versioned top-level directory, and the payload's contents are
    /// what belongs at the toolchain prefix — `rustc/` and `cargo/` carry the
    /// binaries, `rust-std-<triple>/` carries the standard library under
    /// `lib/rustlib/<triple>/lib`.
    fn rust_fixture(triple: &str, std_payload: bool) -> (String, HashMap<String, Vec<u8>>) {
        let version = "1.81.0".to_string();
        let base = "http://127.0.0.1:9";
        let (rustc_name, cargo_name, std_name) = rust::component_filenames(&version, triple);
        let rustc = archive_of(&[
            (
                format!("rustc-{version}-{triple}/rustc/bin/rustc"),
                b"#!/bin/sh\n".to_vec(),
            ),
            (
                format!("rustc-{version}-{triple}/rustc/lib/librustc_driver.so"),
                b"driver".to_vec(),
            ),
            (
                format!("rustc-{version}-{triple}/install.sh"),
                b"#!/bin/sh\n".to_vec(),
            ),
        ]);
        let cargo = archive_of(&[(
            format!("cargo-{version}-{triple}/cargo/bin/cargo"),
            b"#!/bin/sh\n".to_vec(),
        )]);
        let std = if std_payload {
            archive_of(&[(
                format!(
                    "rust-std-{version}-{triple}/rust-std-{triple}/lib/rustlib/{triple}/lib/libstd.rlib"
                ),
                b"std".to_vec(),
            )])
        } else {
            archive_of(&[(
                format!("rust-std-{version}-{triple}/components"),
                b"rust-std\n".to_vec(),
            )])
        };

        let mut files = HashMap::new();
        files.insert(
            format!("{base}/channel-rust-stable.toml"),
            format!("[pkg.rustc]\nversion = \"{version} (abcdef 2024-09-04)\"\n").into_bytes(),
        );
        for (name, body) in [
            (&rustc_name, &rustc),
            (&cargo_name, &cargo),
            (&std_name, &std),
        ] {
            files.insert(format!("{base}/{name}"), body.clone());
            files.insert(
                format!("{base}/{name}.sha256"),
                format!("{}  {name}\n", hex_of(body)).into_bytes(),
            );
        }
        (version, files)
    }

    fn rust_endpoints() -> Endpoints {
        Endpoints {
            node_index_url: "http://127.0.0.1:9/index.json".into(),
            python_release_url: "http://127.0.0.1:9/python.json".into(),
            go_index_url: "http://127.0.0.1:9/go.json".into(),
            rust_dist_url: "http://127.0.0.1:9".into(),
        }
    }

    #[test]
    fn three_rust_archives_assemble_one_toolchain_prefix() {
        let triple = rust_triple().unwrap();
        let (version, files) = rust_fixture(triple, true);
        let client = MapClient {
            files,
            hits: Mutex::new(Vec::new()),
        };
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();

        ensure(
            RuntimeKind::Rust,
            &hint::Scope::app(root.path()),
            cache.path(),
            &client,
            &rust_endpoints(),
            true,
        )
        .unwrap();

        let prefix = cache.path().join("runtimes").join("rust").join(&version);
        assert!(
            prefix.join("bin").join("rustc").is_file(),
            "rustc belongs at the prefix root, not under its component directory"
        );
        assert!(
            prefix.join("bin").join("cargo").is_file(),
            "cargo belongs at the prefix root"
        );
        assert!(
            prefix.join("lib").join("librustc_driver.so").is_file(),
            "the compiler's libraries belong under the prefix lib/"
        );
        assert!(
            prefix
                .join("lib")
                .join("rustlib")
                .join(triple)
                .join("lib")
                .join("libstd.rlib")
                .is_file(),
            "the standard library must land at lib/rustlib/{triple}/lib"
        );
        assert!(prefix.join(".srvm-ok").is_file());
    }

    #[test]
    fn a_rust_prefix_without_its_standard_library_is_not_published() {
        let triple = rust_triple().unwrap();
        let (version, files) = rust_fixture(triple, false);
        let client = MapClient {
            files,
            hits: Mutex::new(Vec::new()),
        };
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();

        let result = ensure(
            RuntimeKind::Rust,
            &hint::Scope::app(root.path()),
            cache.path(),
            &client,
            &rust_endpoints(),
            true,
        );

        let prefix = cache.path().join("runtimes").join("rust").join(&version);
        assert!(
            result.is_err(),
            "a toolchain missing its standard library must not be accepted"
        );
        assert!(
            !prefix.join(".srvm-ok").is_file(),
            "an incomplete prefix must never be published as cached"
        );
    }

    /// Counts what it serves and slows down, so two provisions overlap on
    /// purpose instead of by luck.
    struct CountingClient {
        files: HashMap<String, Vec<u8>>,
        hits: Mutex<Vec<String>>,
        delay: std::time::Duration,
    }

    impl HttpGet for CountingClient {
        fn get(&self, url: &str) -> Result<Vec<u8>> {
            self.hits.lock().unwrap().push(url.to_string());
            std::thread::sleep(self.delay);
            self.files
                .get(url)
                .cloned()
                .with_context(|| format!("missing fixture {url}"))
        }
    }

    #[test]
    fn concurrent_provisions_of_one_version_install_it_once() {
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
            format!("{}  {filename}\n", hex_of(&archive)).into_bytes(),
        );
        files.insert(format!("{base}/{version}/{filename}"), archive);
        let client = CountingClient {
            files,
            hits: Mutex::new(Vec::new()),
            delay: std::time::Duration::from_millis(200),
        };
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let endpoints = rust_endpoints();

        std::thread::scope(|scope| {
            let run = || {
                ensure(
                    RuntimeKind::Node,
                    &hint::Scope::app(root.path()),
                    cache.path(),
                    &client,
                    &endpoints,
                    true,
                )
            };
            let first = scope.spawn(run);
            let second = scope.spawn(run);
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });

        let hits = client.hits.lock().unwrap();
        let downloads = hits.iter().filter(|url| url.ends_with(&filename)).count();
        assert_eq!(
            downloads, 1,
            "a second provisioner must reuse the installed runtime instead of fetching it again: {hits:?}"
        );
    }
}
