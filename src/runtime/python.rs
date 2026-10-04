use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::node::decode_sha256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonChoice {
    pub version: String,
    pub filename: String,
    pub url: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Deserialize)]
struct Release {
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub fn select_python(release_json: &str, want: Option<&str>, triple: &str) -> Result<PythonChoice> {
    let release: Release =
        serde_json::from_str(release_json).context("python release JSON is invalid")?;
    let mut matches = release
        .assets
        .into_iter()
        .filter_map(|asset| candidate(asset, triple))
        .filter(|asset| want.is_none_or(|wanted| version_matches(&asset.version, wanted)))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| cmp_version(&right.version, &left.version));
    let Some(choice) = matches.into_iter().next() else {
        let label = want.unwrap_or("latest stable");
        bail!("python {label} has no install_only build for {triple}");
    };
    let sha256 = decode_sha256(choice.digest.as_deref().unwrap_or(""))
        .with_context(|| format!("python asset {} has no sha256 digest", choice.filename))?;
    Ok(PythonChoice {
        version: choice.version,
        filename: choice.filename,
        url: choice.url,
        sha256,
    })
}

struct Candidate {
    version: String,
    filename: String,
    url: String,
    digest: Option<String>,
}

fn candidate(asset: Asset, triple: &str) -> Option<Candidate> {
    let name = asset.name;
    if name.contains("freethreaded") || name.contains("debug") {
        return None;
    }
    let suffix = format!("-{triple}-install_only.tar.gz");
    if !name.ends_with(&suffix) {
        return None;
    }
    let version = stable_version(&name)?;
    Some(Candidate {
        version,
        filename: name,
        url: asset.browser_download_url,
        digest: asset.digest,
    })
}

fn stable_version(name: &str) -> Option<String> {
    let rest = name.strip_prefix("cpython-")?;
    let (version, _) = rest.split_once('+')?;
    let mut parts = version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    let patch = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if [major, minor, patch]
        .into_iter()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        Some(version.to_string())
    } else {
        None
    }
}

fn version_matches(version: &str, want: &str) -> bool {
    version == want || version.starts_with(&format!("{want}."))
}

fn cmp_version(left: &str, right: &str) -> std::cmp::Ordering {
    let left = version_tuple(left);
    let right = version_tuple(right);
    left.cmp(&right)
}

fn version_tuple(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
      "tag_name": "20261004",
      "assets": [
        {
          "name": "cpython-3.13.1+20261004-aarch64-apple-darwin-install_only.tar.gz",
          "browser_download_url": "https://example.test/3.13.1.tar.gz",
          "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        },
        {
          "name": "cpython-3.13.1rc1+20261004-aarch64-apple-darwin-install_only.tar.gz",
          "browser_download_url": "https://example.test/rc.tar.gz",
          "digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        },
        {
          "name": "cpython-3.12.8+20261004-aarch64-apple-darwin-install_only.tar.gz",
          "browser_download_url": "https://example.test/3.12.8.tar.gz",
          "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        },
        {
          "name": "cpython-3.12.7+20261004-aarch64-apple-darwin-install_only.tar.gz",
          "browser_download_url": "https://example.test/3.12.7.tar.gz",
          "digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
        },
        {
          "name": "cpython-3.12.8+20261004-aarch64-apple-darwin-install_only_stripped.tar.gz",
          "browser_download_url": "https://example.test/stripped.tar.gz",
          "digest": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
        },
        {
          "name": "cpython-3.12.8+20261004-aarch64-apple-darwin-freethreaded+install_only.tar.gz",
          "browser_download_url": "https://example.test/free.tar.gz",
          "digest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        },
        {
          "name": "cpython-3.13.1+20261004-x86_64_v3-unknown-linux-gnu-install_only.tar.gz",
          "browser_download_url": "https://example.test/v3.tar.gz",
          "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111"
        },
        {
          "name": "cpython-3.12.8+20261004-x86_64-apple-darwin-install_only.tar.gz",
          "browser_download_url": "https://example.test/x64.tar.gz",
          "digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222"
        }
      ]
    }"#;

    #[test]
    fn default_picks_newest_stable_install_only_for_triple() {
        let choice = select_python(RELEASE, None, "aarch64-apple-darwin").unwrap();
        assert_eq!(choice.version, "3.13.1");
        assert_eq!(
            choice.filename,
            "cpython-3.13.1+20261004-aarch64-apple-darwin-install_only.tar.gz"
        );
        assert_eq!(choice.url, "https://example.test/3.13.1.tar.gz");
        assert_eq!(
            hex(&choice.sha256),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn prefix_and_exact_want() {
        let minor = select_python(RELEASE, Some("3.12"), "aarch64-apple-darwin").unwrap();
        assert_eq!(minor.version, "3.12.8");

        let exact = select_python(RELEASE, Some("3.12.7"), "aarch64-apple-darwin").unwrap();
        assert_eq!(exact.version, "3.12.7");
        assert_eq!(exact.url, "https://example.test/3.12.7.tar.gz");
    }

    #[test]
    fn rejects_other_triples_and_missing_versions() {
        assert!(select_python(RELEASE, Some("3.13.1"), "x86_64-pc-windows-msvc").is_err());
        let err = select_python(RELEASE, Some("3.11"), "aarch64-apple-darwin").unwrap_err();
        assert!(err.to_string().contains("3.11"), "{err}");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
