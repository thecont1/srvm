use anyhow::{Context, Result};
use serde::Deserialize;

use super::hint::GoWant;
use super::node::decode_sha256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoChoice {
    pub version: String,
    pub filename: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Deserialize)]
struct Release {
    version: String,
    stable: bool,
    files: Vec<File>,
}

#[derive(Debug, Deserialize)]
struct File {
    filename: String,
    os: String,
    arch: String,
    sha256: String,
    kind: String,
}

pub fn select_go(
    index_json: &str,
    want: Option<&GoWant>,
    os: &str,
    arch: &str,
) -> Result<GoChoice> {
    let releases: Vec<Release> =
        serde_json::from_str(index_json).context("go release index is not valid JSON")?;
    let mut matches = releases
        .into_iter()
        .filter(|release| release.stable && version_matches(&release.version, want))
        .filter_map(|release| archive_for(release, os, arch))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| cmp_go(&right.version, &left.version));
    matches.into_iter().next().ok_or_else(|| {
        let label = match want {
            Some(GoWant::Exact(version) | GoWant::Prefix(version)) => version.as_str(),
            None => "latest stable",
        };
        anyhow::anyhow!("go {label} has no archive for {os}-{arch}")
    })
}

fn version_matches(release: &str, want: Option<&GoWant>) -> bool {
    let version = release.trim_start_matches("go");
    match want {
        None => !version.contains("rc") && !version.contains("beta"),
        Some(GoWant::Exact(wanted)) => version == wanted,
        Some(GoWant::Prefix(prefix)) => {
            version == prefix || version.starts_with(&format!("{prefix}."))
        }
    }
}

fn archive_for(release: Release, os: &str, arch: &str) -> Option<GoChoice> {
    let file = release.files.into_iter().find(|file| {
        file.kind == "archive"
            && file.os == os
            && file.arch == arch
            && !file.filename.contains("installer")
    })?;
    let sha256 = decode_sha256(&file.sha256).ok()?;
    Some(GoChoice {
        version: release.version,
        filename: file.filename,
        sha256,
    })
}

fn cmp_go(left: &str, right: &str) -> std::cmp::Ordering {
    go_tuple(left).cmp(&go_tuple(right))
}

fn go_tuple(version: &str) -> Vec<u32> {
    version
        .trim_start_matches("go")
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(os: &str, arch: &str) -> String {
        format!(
            r#"[
              {{"version":"go1.23.4","stable":true,"files":[
                {{"filename":"go1.23.4.{os}-{arch}.tar.gz","os":"{os}","arch":"{arch}","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","kind":"archive"}},
                {{"filename":"go1.23.4.src.tar.gz","os":"","arch":"","sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","kind":"source"}}
              ]}},
              {{"version":"go1.23.5rc1","stable":false,"files":[
                {{"filename":"go1.23.5rc1.{os}-{arch}.tar.gz","os":"{os}","arch":"{arch}","sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","kind":"archive"}}
              ]}},
              {{"version":"go1.22.10","stable":true,"files":[
                {{"filename":"go1.22.10.{os}-{arch}.tar.gz","os":"{os}","arch":"{arch}","sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","kind":"archive"}}
              ]}}
            ]"#
        )
    }

    #[test]
    fn default_skips_prerelease_and_source() {
        let choice = select_go(&index("darwin", "arm64"), None, "darwin", "arm64").unwrap();
        assert_eq!(choice.version, "go1.23.4");
        assert_eq!(choice.filename, "go1.23.4.darwin-arm64.tar.gz");
    }

    #[test]
    fn prefix_picks_newest_patch_and_exact_is_strict() {
        let minor = select_go(
            &index("linux", "amd64"),
            Some(&GoWant::Prefix("1.22".into())),
            "linux",
            "amd64",
        )
        .unwrap();
        assert_eq!(minor.version, "go1.22.10");

        let exact = select_go(
            &index("linux", "amd64"),
            Some(&GoWant::Exact("1.22.10".into())),
            "linux",
            "amd64",
        )
        .unwrap();
        assert_eq!(exact.filename, "go1.22.10.linux-amd64.tar.gz");

        let err = select_go(
            &index("linux", "amd64"),
            Some(&GoWant::Exact("1.21.0".into())),
            "linux",
            "amd64",
        )
        .unwrap_err();
        assert!(err.to_string().contains("1.21.0"), "{err}");
    }
}
