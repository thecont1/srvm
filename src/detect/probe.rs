use std::{fs, io::Read, path::Path};

use anyhow::{Result, bail};
use serde_json::Value;

const PROBE_CAP: u64 = 1_048_576;

pub fn file_exists(root: &Path, rel: &str) -> bool {
    root.join(rel).is_file()
}

pub fn dir_exists(root: &Path, rel: &str) -> bool {
    root.join(rel).is_dir()
}

pub fn file_contains(root: &Path, rel: &str, needle: &str) -> bool {
    read_capped(&root.join(rel))
        .map(|text| text.contains(needle))
        .unwrap_or(false)
}

/// Reads the `package.name` from a `Cargo.toml` using a lossy, line-based scan,
/// so a malformed or oversized manifest degrades to `None` instead of failing the
/// whole detection pass. This is just for self-identification — srvm must not
/// launch itself — not for authoring or publishing Cargo metadata.
pub fn cargo_package_name(root: &Path) -> Option<String> {
    let text = read_lossy(root, "Cargo.toml")?;
    let mut in_package = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            // Only `[package]` owns a top-level `name`; a virtual workspace
            // (`[workspace]`) has none and the rule's `src/main.rs` guard
            // already keeps it from matching here anyway.
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim().trim_matches('"');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

pub fn read_to_string(root: &Path, rel: &str) -> Result<String> {
    let path = root.join(rel);
    let len = fs::metadata(&path)?.len();
    if len > PROBE_CAP {
        bail!("{rel} is {len} bytes, above the {PROBE_CAP}-byte marker cap");
    }
    Ok(fs::read_to_string(path)?)
}

/// Capped, lossy file read for marker scanning — returns None when the file
/// cannot be read instead of failing the whole detection pass.
pub fn read_lossy(root: &Path, rel: &str) -> Option<String> {
    read_capped(&root.join(rel)).ok()
}

pub fn read_jsonc(root: &Path, rel: &str) -> Result<Value> {
    let text = read_to_string(root, rel)?;
    Ok(serde_json::from_str(&strip_jsonc(&text))?)
}

pub fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if prev == '*' && next == '/' {
                        break;
                    }
                    prev = next;
                }
            }
            _ => out.push(ch),
        }
    }

    remove_trailing_commas(&out)
}

fn remove_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }

        if ch == ',' {
            let mut clone = chars.clone();
            while matches!(clone.peek(), Some(c) if c.is_whitespace()) {
                clone.next();
            }
            if matches!(clone.peek(), Some('}' | ']')) {
                continue;
            }
        }

        out.push(ch);
    }

    out
}

fn read_capped(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(PROBE_CAP).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{cargo_package_name, strip_jsonc};

    #[test]
    fn cargo_package_name_reads_the_manifest_package_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"srvm\"\nversion = \"0.1.2\"\n",
        )
        .unwrap();
        assert_eq!(cargo_package_name(dir.path()), Some("srvm".to_string()));
    }

    #[test]
    fn cargo_package_name_ignores_a_name_outside_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "// leading comment\n[workspace]\nname = \"workspace-name\"\n[package]\nname = \"app\"\n",
        )
        .unwrap();
        assert_eq!(cargo_package_name(dir.path()), Some("app".to_string()));
    }

    #[test]
    fn cargo_package_name_handles_inline_tables_and_spacing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package] # comment\n  name   =   \"app\"  # trailing\n",
        )
        .unwrap();
        assert_eq!(cargo_package_name(dir.path()), Some("app".to_string()));
    }

    #[test]
    fn cargo_package_name_missing_manifest_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cargo_package_name(dir.path()), None);
    }

    #[test]
    fn strips_comments_and_trailing_commas() {
        let parsed: serde_json::Value = serde_json::from_str(&strip_jsonc(
            r#"{
              // line
              "url": "https://example.com//keep",
              "items": [1, 2,],
              /* block */
            }"#,
        ))
        .unwrap();

        assert_eq!(parsed["items"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["url"], "https://example.com//keep");
    }
}
