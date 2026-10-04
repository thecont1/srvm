use anyhow::{Result, bail};

pub fn channel_filename(channel: Option<&str>) -> Result<String> {
    let channel = channel.unwrap_or("stable");
    if !channel_ok(channel) {
        bail!("refusing rust channel {channel:?}");
    }
    Ok(format!("channel-rust-{channel}.toml"))
}

pub fn component_filenames(version: &str, triple: &str) -> (String, String) {
    (
        format!("rustc-{version}-{triple}.tar.gz"),
        format!("cargo-{version}-{triple}.tar.gz"),
    )
}

pub fn version_from_channel_toml(text: &str) -> Result<String> {
    let mut in_rustc = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_rustc = line == "[pkg.rustc]";
            continue;
        }
        if !in_rustc {
            continue;
        }
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let value = rest.trim().trim_start_matches('=').trim().trim_matches('"');
        let version = value.split_whitespace().next().unwrap_or("");
        if version.split('.').count() == 3
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        {
            return Ok(version.to_string());
        }
        bail!("rustc version {value:?} is not an X.Y.Z release");
    }
    bail!("channel manifest has no [pkg.rustc] version")
}

fn channel_ok(channel: &str) -> bool {
    !channel.is_empty()
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_file_and_component_names() {
        assert_eq!(channel_filename(None).unwrap(), "channel-rust-stable.toml");
        assert_eq!(
            channel_filename(Some("1.81.0")).unwrap(),
            "channel-rust-1.81.0.toml"
        );
        assert!(channel_filename(Some("../stable")).is_err());
        assert_eq!(
            component_filenames("1.81.0", "aarch64-apple-darwin"),
            (
                "rustc-1.81.0-aarch64-apple-darwin.tar.gz".into(),
                "cargo-1.81.0-aarch64-apple-darwin.tar.gz".into()
            )
        );
    }

    #[test]
    fn reads_rustc_version_from_channel_manifest() {
        let text = r#"
            manifest-version = "2"
            [pkg.cargo]
            version = "0.0.0 (ignored)"
            [pkg.rustc]
            version = "1.81.0 (eeb90cda 2024-09-04)"
        "#;
        assert_eq!(version_from_channel_toml(text).unwrap(), "1.81.0");
    }
}
