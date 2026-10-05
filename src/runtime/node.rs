use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use super::hint::NodeWant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeChoice {
    pub version: String,
    pub filename: String,
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    version: String,
    lts: Value,
}

pub fn select_node(
    index_json: &str,
    want: Option<&NodeWant>,
    target: &str,
    archive_ext: &str,
) -> Result<NodeChoice> {
    let entries: Vec<IndexEntry> =
        serde_json::from_str(index_json).context("node index is not valid JSON")?;
    let version = pick_node_version(&entries, want)?;
    Ok(NodeChoice {
        filename: format!("node-{version}-{target}.{archive_ext}"),
        version,
    })
}

fn pick_node_version(entries: &[IndexEntry], want: Option<&NodeWant>) -> Result<String> {
    let found = match want {
        Some(NodeWant::Latest) => entries.first().map(|entry| entry.version.clone()),
        Some(NodeWant::Exact(version)) => {
            let wanted = normalize_exact(version);
            entries.iter().find_map(|entry| {
                (normalize_exact(&entry.version) == wanted).then(|| entry.version.clone())
            })
        }
        Some(NodeWant::Prefix(prefix)) => entries.iter().find_map(|entry| {
            version_has_prefix(&entry.version, prefix).then(|| entry.version.clone())
        }),
        Some(NodeWant::Lts(Some(name))) => entries.iter().find_map(|entry| {
            entry
                .lts
                .as_str()
                .is_some_and(|lts| lts.eq_ignore_ascii_case(name))
                .then(|| entry.version.clone())
        }),
        None | Some(NodeWant::Lts(None)) => entries
            .iter()
            .find_map(|entry| entry.lts.as_str().is_some().then(|| entry.version.clone())),
    };
    found.ok_or_else(|| match want {
        Some(NodeWant::Exact(version)) => anyhow::anyhow!("node {version} is not in the index"),
        Some(NodeWant::Prefix(prefix)) => anyhow::anyhow!("node {prefix} is not in the index"),
        Some(NodeWant::Lts(Some(name))) => anyhow::anyhow!("node lts/{name} is not in the index"),
        _ => anyhow::anyhow!("node index has no matching release"),
    })
}

fn normalize_exact(version: &str) -> String {
    format!("v{}", version.trim().trim_start_matches('v'))
}

fn version_has_prefix(version: &str, prefix: &str) -> bool {
    let version = version.trim_start_matches('v');
    version == prefix || version.starts_with(&format!("{prefix}."))
}

pub fn sha256_for(shasums: &str, filename: &str) -> Result<[u8; 32]> {
    for line in shasums.lines() {
        let mut parts = line.split_whitespace();
        let Some(hex) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        if name == filename {
            return decode_sha256(hex)
                .with_context(|| format!("checksum for {filename} is not sha256"));
        }
    }
    bail!("no checksum for {filename}")
}

pub fn decode_sha256(hex: &str) -> Result<[u8; 32]> {
    let hex = hex.trim().trim_start_matches("sha256:");
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected 64 hex characters, got {hex}");
    }
    let mut out = [0u8; 32];
    for (idx, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[idx * 2..idx * 2 + 2], 16)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::hint::NodeWant;

    const INDEX: &str = r#"[
      {"version":"v23.5.0","lts":false},
      {"version":"v22.21.0","lts":"Jod"},
      {"version":"v22.14.0","lts":"Jod"},
      {"version":"v20.18.1","lts":"Iron"}
    ]"#;

    fn choose(want: Option<NodeWant>) -> NodeChoice {
        select_node(INDEX, want.as_ref(), "darwin-arm64", "tar.gz").unwrap()
    }

    #[test]
    fn default_is_newest_lts_not_current() {
        let choice = choose(None);
        assert_eq!(choice.version, "v22.21.0");
        assert_eq!(choice.filename, "node-v22.21.0-darwin-arm64.tar.gz");
    }

    #[test]
    fn exact_prefix_lts_codename_and_latest() {
        assert_eq!(
            choose(Some(NodeWant::Exact("v20.18.1".into()))).version,
            "v20.18.1"
        );
        assert_eq!(
            choose(Some(NodeWant::Prefix("22".into()))).version,
            "v22.21.0"
        );
        assert_eq!(
            choose(Some(NodeWant::Prefix("22.14".into()))).version,
            "v22.14.0"
        );
        assert_eq!(
            choose(Some(NodeWant::Lts(Some("iron".into())))).version,
            "v20.18.1"
        );
        assert_eq!(choose(Some(NodeWant::Latest)).version, "v23.5.0");
    }

    #[test]
    fn windows_filename_uses_zip() {
        let choice = select_node(
            INDEX,
            Some(&NodeWant::Exact("v22.21.0".into())),
            "win-x64",
            "zip",
        )
        .unwrap();
        assert_eq!(choice.filename, "node-v22.21.0-win-x64.zip");
    }

    #[test]
    fn missing_exact_version_errors() {
        let err = select_node(
            INDEX,
            Some(&NodeWant::Exact("v1.2.3".into())),
            "darwin-arm64",
            "tar.gz",
        )
        .unwrap_err();
        assert!(err.to_string().contains("v1.2.3"), "{err}");
    }

    #[test]
    fn shasums_lookup_is_whitespace_tolerant() {
        let text = "\
aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999  node-v22.21.0-darwin-arm64.tar.gz
deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef node-other.tar.gz
";
        let digest = sha256_for(text, "node-v22.21.0-darwin-arm64.tar.gz").unwrap();
        assert_eq!(
            hex(&digest),
            "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999"
        );
        assert!(sha256_for(text, "missing.tar.gz").is_err());
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
