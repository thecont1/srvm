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
    use super::strip_jsonc;

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
