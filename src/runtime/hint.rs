use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeWant {
    Exact(String),
    Prefix(String),
    Lts(Option<String>),
    Latest,
}

pub fn node_want(root: &Path) -> Option<NodeWant> {
    read_version_file(root, ".nvmrc")
        .or_else(|| read_version_file(root, ".node-version"))
        .map(|raw| parse_node_want(&raw))
}

pub fn go_want(root: &Path) -> Option<GoWant> {
    let text = fs::read_to_string(root.join("go.mod")).ok()?;
    let mut language = None;
    let mut toolchain = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("toolchain ") {
            toolchain = parse_go_version(rest.trim());
        } else if let Some(rest) = line.strip_prefix("go ")
            && !rest.contains('/')
        {
            language = parse_go_version(rest.trim()).map(|want| match want {
                GoWant::Exact(version) | GoWant::Prefix(version) => {
                    let mut parts = version.split('.');
                    let major = parts.next().unwrap_or("");
                    let minor = parts.next().unwrap_or("");
                    GoWant::Prefix(format!("{major}.{minor}"))
                }
            });
        }
    }
    toolchain.or(language)
}

pub fn rust_channel(root: &Path) -> Option<String> {
    read_to_optional(root, "rust-toolchain.toml")
        .as_deref()
        .and_then(channel_from_toml)
        .or_else(|| read_version_file(root, "rust-toolchain").filter(|raw| channel_token_ok(raw)))
}

fn read_to_optional(root: &Path, name: &str) -> Option<String> {
    fs::read_to_string(root.join(name)).ok()
}

fn parse_go_version(raw: &str) -> Option<GoWant> {
    let raw = raw.trim().trim_start_matches("go");
    let mut parts = raw.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if !numeric(major) || !numeric(minor) {
        return None;
    }
    match parts.next() {
        Some(patch) if numeric(patch) && parts.next().is_none() => {
            Some(GoWant::Exact(format!("{major}.{minor}.{patch}")))
        }
        None => Some(GoWant::Prefix(format!("{major}.{minor}"))),
        _ => None,
    }
}

fn numeric(part: &str) -> bool {
    !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())
}

fn channel_from_toml(text: &str) -> Option<String> {
    let mut in_toolchain = false;
    for line in text.lines() {
        let line = strip_toml_comment(line).trim();
        if line.starts_with('[') {
            in_toolchain = line == "[toolchain]";
            continue;
        }
        if !in_toolchain {
            continue;
        }
        let Some(rest) = line.strip_prefix("channel") else {
            continue;
        };
        let value = rest.trim().trim_start_matches('=').trim();
        let value = unquote(value)?;
        if channel_token_ok(&value) {
            return Some(value);
        }
    }
    None
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_quote = false;
    let bytes = line.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' => in_quote = !in_quote,
            b'#' if !in_quote => return &line[..idx],
            _ => {}
        }
        idx += 1;
    }
    line
}

fn unquote(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(',');
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return Some(inner.to_string());
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return Some(inner.to_string());
    }
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn channel_token_ok(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoWant {
    Exact(String),
    Prefix(String),
}

pub fn python_want(root: &Path) -> Option<String> {
    read_version_file(root, ".python-version").filter(|raw| {
        let mut parts = raw.split('.');
        let major = parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        let rest_ok =
            parts.all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        major && rest_ok && raw.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn parse_node_want(raw: &str) -> NodeWant {
    let lower = raw.to_ascii_lowercase();
    if lower == "node" || lower == "latest" {
        return NodeWant::Latest;
    }
    if let Some(codename) = lower.strip_prefix("lts/") {
        let codename = codename.trim();
        if codename.is_empty() || codename == "*" {
            return NodeWant::Lts(None);
        }
        return NodeWant::Lts(Some(codename.to_string()));
    }
    if lower == "lts" || lower == "lts/*" {
        return NodeWant::Lts(None);
    }

    let version = raw.trim().trim_start_matches('v');
    let parts = version.split('.');
    let numeric = parts
        .clone()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    let count = parts.filter(|part| !part.is_empty()).count();
    if numeric && count == 3 {
        return NodeWant::Exact(format!("v{version}"));
    }
    if numeric && (count == 1 || count == 2) {
        return NodeWant::Prefix(version.to_string());
    }
    NodeWant::Latest
}

fn read_version_file(root: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(root.join(name)).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn root(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn nvmrc_exact_prefix_and_lts() {
        let exact = root(&[(".nvmrc", "v22.21.0\n")]);
        assert_eq!(
            node_want(exact.path()),
            Some(NodeWant::Exact("v22.21.0".into()))
        );

        let bare = root(&[(".nvmrc", "22.21.0\n")]);
        assert_eq!(
            node_want(bare.path()),
            Some(NodeWant::Exact("v22.21.0".into()))
        );

        let major = root(&[(".nvmrc", "22\n")]);
        assert_eq!(node_want(major.path()), Some(NodeWant::Prefix("22".into())));

        let minor = root(&[(".nvmrc", "22.14\n")]);
        assert_eq!(
            node_want(minor.path()),
            Some(NodeWant::Prefix("22.14".into()))
        );

        let lts = root(&[(".nvmrc", "lts/*\n")]);
        assert_eq!(node_want(lts.path()), Some(NodeWant::Lts(None)));

        let named = root(&[(".nvmrc", "lts/Jod\n")]);
        assert_eq!(
            node_want(named.path()),
            Some(NodeWant::Lts(Some("jod".into())))
        );

        let latest = root(&[(".nvmrc", "node\n")]);
        assert_eq!(node_want(latest.path()), Some(NodeWant::Latest));
    }

    #[test]
    fn node_version_file_used_when_nvmrc_absent() {
        let dir = root(&[(".node-version", "# comment\n\n20.18.1\n")]);
        assert_eq!(
            node_want(dir.path()),
            Some(NodeWant::Exact("v20.18.1".into()))
        );
    }

    #[test]
    fn nvmrc_wins_over_node_version() {
        let dir = root(&[(".nvmrc", "22\n"), (".node-version", "18.0.0\n")]);
        assert_eq!(node_want(dir.path()), Some(NodeWant::Prefix("22".into())));
    }

    #[test]
    fn python_version_skips_comments() {
        let dir = root(&[(".python-version", "# pyenv\n3.12\n")]);
        assert_eq!(python_want(dir.path()).as_deref(), Some("3.12"));
    }

    #[test]
    fn missing_hint_files_are_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(node_want(dir.path()), None);
        assert_eq!(python_want(dir.path()), None);
        assert_eq!(go_want(dir.path()), None);
        assert_eq!(rust_channel(dir.path()), None);
    }

    #[test]
    fn go_mod_toolchain_wins_over_language_line() {
        let exact = root(&[(
            "go.mod",
            "module example.com/app\n\ngo 1.22.0\ntoolchain go1.22.5\n",
        )]);
        assert_eq!(go_want(exact.path()), Some(GoWant::Exact("1.22.5".into())));

        let minor = root(&[("go.mod", "module example.com/app\n\ngo 1.22\n")]);
        assert_eq!(go_want(minor.path()), Some(GoWant::Prefix("1.22".into())));

        let patch = root(&[("go.mod", "module example.com/app\n\ngo 1.22.0\n")]);
        assert_eq!(go_want(patch.path()), Some(GoWant::Prefix("1.22".into())));
    }

    #[test]
    fn rust_toolchain_toml_wins_over_plain_file() {
        let toml = root(&[("rust-toolchain.toml", "[toolchain]\nchannel = \"1.81.0\"\n")]);
        assert_eq!(rust_channel(toml.path()).as_deref(), Some("1.81.0"));

        let plain = root(&[("rust-toolchain", "nightly\n")]);
        assert_eq!(rust_channel(plain.path()).as_deref(), Some("nightly"));

        let both = root(&[
            (
                "rust-toolchain.toml",
                "channel = \"stable\"\n[toolchain]\nchannel = \"1.80.0\"\n",
            ),
            ("rust-toolchain", "nightly\n"),
        ]);
        assert_eq!(rust_channel(both.path()).as_deref(), Some("1.80.0"));
    }
}
