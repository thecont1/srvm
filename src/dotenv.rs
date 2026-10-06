//! Parse-only `.env` support.
//!
//! A cloned repository should run without the user sourcing anything, so srvm
//! reads `KEY=VALUE` lines itself. Nothing here is evaluated: no `$VAR`
//! expansion, no command substitution, no shell. Values are handed to child
//! processes only for keys the ambient environment does not already define, so
//! the OS environment always wins, and they are never echoed.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Above this size a `.env` is reported and ignored rather than parsed in part.
pub const MAX_ENV_BYTES: u64 = 64 * 1024;

/// Conventional sample files, checked only to explain why nothing was loaded.
pub const SAMPLE_NAMES: &[&str] = &[".env.example", ".env.sample", ".env.template"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnvFile {
    /// Effective pairs in file order; a later duplicate replaces the earlier
    /// value.
    pub pairs: Vec<(String, String)>,
    /// Diagnostics for lines that cannot be used.
    pub notes: Vec<String>,
}

impl EnvFile {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".env")
    }
}

pub fn load(root: &Path) -> EnvFile {
    let path = EnvFile::path(root);
    let Ok(meta) = fs::metadata(&path) else {
        return EnvFile::default();
    };
    if meta.len() > MAX_ENV_BYTES {
        return EnvFile {
            pairs: Vec::new(),
            notes: vec![format!(
                ".env is {} bytes, above the {MAX_ENV_BYTES} byte cap; ignoring it",
                meta.len()
            )],
        };
    }
    match fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(err) => EnvFile {
            pairs: Vec::new(),
            notes: vec![format!("could not read .env: {err}")],
        },
    }
}

/// The sample file an app ships when it has no `.env`. srvm will not guess
/// configuration from an example, but it says so instead of staying silent.
pub fn sample_without_env(root: &Path) -> Option<&'static str> {
    if fs::metadata(EnvFile::path(root)).is_ok() {
        return None;
    }
    SAMPLE_NAMES
        .iter()
        .find(|name| root.join(name).is_file())
        .copied()
}

pub fn parse(text: &str) -> EnvFile {
    let mut file = EnvFile::default();

    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = strip_export(line);
        let Some((key, value)) = line.split_once('=') else {
            let line_number = number + 1;
            file.notes
                .push(format!("line {line_number}: not KEY=VALUE, ignored"));
            continue;
        };

        let key = key.trim();
        if !is_key(key) {
            let line_number = number + 1;
            file.notes.push(format!(
                "line {line_number}: {key:?} is not a name, ignored"
            ));
            continue;
        }

        let value = parse_value(value.trim(), &mut file.notes, number + 1);
        match file.pairs.iter_mut().find(|(name, _)| name == key) {
            Some(existing) => existing.1 = value,
            None => file.pairs.push((key.to_string(), value)),
        }
    }

    file
}

/// Pairs to inject into a child started in `root`: `.env` entries whose key
/// the ambient environment does not already define.
pub fn child_env(root: &Path) -> (Vec<(String, String)>, Vec<String>) {
    child_env_with(root, |key| env::var_os(key).is_some())
}

pub fn child_env_with(
    root: &Path,
    is_set: impl Fn(&str) -> bool,
) -> (Vec<(String, String)>, Vec<String>) {
    let file = load(root);
    let pairs = file
        .pairs
        .into_iter()
        .filter(|(key, _)| !is_set(key))
        .collect();
    (pairs, file.notes)
}

fn strip_export(line: &str) -> &str {
    match line.strip_prefix("export") {
        Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
        _ => line,
    }
}

fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn parse_value(raw: &str, notes: &mut Vec<String>, line: usize) -> String {
    if let Some(quoted) = raw.strip_prefix('"') {
        return match quoted.strip_suffix('"') {
            Some(inner) => unescape_double(inner),
            None => {
                notes.push(format!(
                    "line {line}: unterminated double quote, using the rest verbatim"
                ));
                quoted.to_string()
            }
        };
    }
    if let Some(quoted) = raw.strip_prefix('\'') {
        return match quoted.strip_suffix('\'') {
            Some(inner) => inner.to_string(),
            None => {
                notes.push(format!(
                    "line {line}: unterminated single quote, using the rest verbatim"
                ));
                quoted.to_string()
            }
        };
    }

    // An unquoted value ends at a comment introduced by whitespace, so
    // `PORT=3000 # dev` is 3000 while `GREETING=hello world` keeps its space.
    let value = raw
        .char_indices()
        .find(|&(index, ch)| ch == '#' && index > 0 && raw[..index].ends_with([' ', '\t']))
        .map_or(raw, |(index, _)| &raw[..index]);
    value.trim_end().to_string()
}

fn unescape_double(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            // An unknown escape stays verbatim: nothing is interpreted.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse(text).pairs
    }

    #[test]
    fn a_tab_before_a_comment_hash_ends_the_value_too() {
        assert_eq!(
            pairs("PORT=3000\t# dev\nGREETING=hello world\n"),
            vec![
                ("PORT".to_string(), "3000".to_string()),
                ("GREETING".to_string(), "hello world".to_string()),
            ]
        );
    }

    #[test]
    fn reads_plain_and_exported_assignments() {
        assert_eq!(
            pairs("A=1\nB=two\nexport C=3\n"),
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two".into()),
                ("C".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        assert_eq!(
            pairs("# a comment\n\nA=1\n   \n"),
            vec![("A".into(), "1".into())]
        );
    }

    #[test]
    fn strips_quotes_and_handles_double_quote_escapes() {
        assert_eq!(
            pairs(r#"A="hello world""#),
            vec![("A".into(), "hello world".into())]
        );
        assert_eq!(
            pairs("A='single quoted'"),
            vec![("A".into(), "single quoted".into())]
        );
        assert_eq!(
            pairs(r#"A="line\nbreak\t\"quoted\"""#),
            vec![("A".into(), "line\nbreak\t\"quoted\"".into())]
        );
    }

    #[test]
    fn unquoted_values_keep_spaces_but_drop_trailing_comments() {
        assert_eq!(
            pairs("GREETING=hello world\nPORT=3000 # dev\n"),
            vec![
                ("GREETING".into(), "hello world".into()),
                ("PORT".into(), "3000".into()),
            ]
        );
    }

    #[test]
    fn never_expands_or_interprets_values() {
        assert_eq!(
            pairs("A=$HOME/x\nB=`whoami`\nC=$(id)\n"),
            vec![
                ("A".into(), "$HOME/x".into()),
                ("B".into(), "`whoami`".into()),
                ("C".into(), "$(id)".into()),
            ]
        );
    }

    #[test]
    fn tolerates_whitespace_crlf_and_empty_values() {
        assert_eq!(
            pairs("A = 1\r\nB=\r\n"),
            vec![("A".into(), "1".into()), ("B".into(), "".into())]
        );
    }

    #[test]
    fn later_duplicates_win() {
        assert_eq!(pairs("A=1\nA=2\n"), vec![("A".into(), "2".into())]);
    }

    #[test]
    fn unusable_lines_are_noted_not_fatal() {
        let file = parse("1A=1\nJUSTAWORD\nB=2\n");
        assert_eq!(file.pairs, vec![("B".into(), "2".into())]);
        assert_eq!(file.notes.len(), 2, "{:?}", file.notes);
        assert!(file.notes.iter().any(|note| note.contains("line 1")));
        assert!(file.notes.iter().any(|note| note.contains("line 2")));
    }

    #[test]
    fn unterminated_quote_is_noted_and_used_verbatim() {
        let file = parse("A=\"oops\n");
        assert_eq!(file.pairs, vec![("A".into(), "oops".into())]);
        assert!(file.notes.iter().any(|note| note.contains("unterminated")));
    }

    #[test]
    fn a_missing_file_is_an_empty_environment() {
        let dir = tempdir().unwrap();
        assert_eq!(load(dir.path()), EnvFile::default());
    }

    #[test]
    fn an_oversized_file_is_ignored_whole() {
        let dir = tempdir().unwrap();
        let mut text = String::from("A=1\n");
        text.push_str(&"#".repeat(MAX_ENV_BYTES as usize));
        fs::write(dir.path().join(".env"), text).unwrap();

        let file = load(dir.path());

        assert!(file.pairs.is_empty(), "{:?}", file.pairs);
        assert!(file.notes.iter().any(|note| note.contains("cap")));
    }

    #[test]
    fn a_sample_without_a_dotenv_is_reported() {
        let dir = tempdir().unwrap();
        assert_eq!(sample_without_env(dir.path()), None);

        fs::write(dir.path().join(".env.example"), "A=1\n").unwrap();
        assert_eq!(sample_without_env(dir.path()), Some(".env.example"));

        fs::write(dir.path().join(".env"), "A=2\n").unwrap();
        assert_eq!(sample_without_env(dir.path()), None);
    }

    #[test]
    fn ambient_environment_wins_over_the_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".env"), "KEEP=file\nOVERRIDE=file\n").unwrap();

        let (pairs, notes) = child_env_with(dir.path(), |key| key == "OVERRIDE");

        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pairs, vec![("KEEP".into(), "file".into())]);
    }
}
