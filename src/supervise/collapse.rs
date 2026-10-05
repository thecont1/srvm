use regex::Regex;
use std::sync::OnceLock;

static QUOTED_RE: OnceLock<Regex> = OnceLock::new();
static PATH_RE: OnceLock<Regex> = OnceLock::new();
static NUMBER_RE: OnceLock<Regex> = OnceLock::new();

#[derive(Debug, Default)]
pub struct Collapser {
    last_key: Option<String>,
    repeats: usize,
    hidden: usize,
}

impl Collapser {
    pub fn accept(&mut self, line: &str) -> Option<String> {
        let key = noise_key(line);
        if self.last_key.as_deref() == Some(&key) {
            self.repeats += 1;
            if self.repeats <= 2 {
                return Some(line.to_string());
            }
            self.hidden += 1;
            return None;
        }

        let folded = self.flush();
        self.last_key = Some(key);
        self.repeats = 1;
        self.hidden = 0;

        folded.or_else(|| Some(line.to_string()))
    }

    pub fn flush(&mut self) -> Option<String> {
        if self.hidden == 0 {
            return None;
        }
        let hidden = self.hidden;
        self.hidden = 0;
        Some(format!("· {hidden} more like the above"))
    }
}

fn noise_key(line: &str) -> String {
    let line = QUOTED_RE
        .get_or_init(|| Regex::new(r#""[^"]*"|'[^']*'"#).unwrap())
        .replace_all(line, "<quoted>");
    let line = PATH_RE
        .get_or_init(|| Regex::new(r"(?:[A-Za-z]:)?[/\\][^\s]+(?:[/\\][^\s]+)*").unwrap())
        .replace_all(&line, "<path>");
    NUMBER_RE
        .get_or_init(|| Regex::new(r"\d+").unwrap())
        .replace_all(&line, "<n>")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::Collapser;

    #[test]
    fn folds_repeated_noise_after_two_lines() {
        let mut collapse = Collapser::default();
        assert!(collapse.accept("compiled /tmp/a in 1ms").is_some());
        assert!(collapse.accept("compiled /tmp/b in 2ms").is_some());
        assert!(collapse.accept("compiled /tmp/c in 3ms").is_none());
        assert_eq!(collapse.flush(), Some("· 1 more like the above".into()));
    }
}
