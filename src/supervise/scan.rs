use regex::Regex;
use std::sync::OnceLock;

static URL_RE: OnceLock<Regex> = OnceLock::new();
static BARE_RE: OnceLock<Regex> = OnceLock::new();
static ANSI_RE: OnceLock<Regex> = OnceLock::new();

pub fn sniff_url(line: &str) -> Option<String> {
    let clean = strip_ansi(line);

    if let Some(caps) = URL_RE
        .get_or_init(|| Regex::new(r"(?i)https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|::1)(?::\d+)?(?:/[^\s]*)?").unwrap())
        .captures(&clean)
    {
        return normalize_url(caps.get(0)?.as_str());
    }

    if let Some(caps) = BARE_RE
        .get_or_init(|| {
            Regex::new(r"(?i)(?:localhost|127\.0\.0\.1|0\.0\.0\.0)(:\d+)(?:/[^\s]*)?").unwrap()
        })
        .captures(&clean)
    {
        return normalize_url(caps.get(0)?.as_str());
    }

    None
}

pub fn strip_ansi(line: &str) -> String {
    ANSI_RE
        .get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap())
        .replace_all(line, "")
        .into_owned()
}

fn normalize_url(raw: &str) -> Option<String> {
    let mut url = raw
        .trim_matches(|ch: char| ch == ')' || ch == ']' || ch == '}' || ch == ',' || ch == '.')
        .to_string();

    if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }

    url = url
        .replace("http://0.0.0.0", "http://127.0.0.1")
        .replace("https://0.0.0.0", "https://127.0.0.1")
        .replace("http://[::1]", "http://127.0.0.1")
        .replace("https://[::1]", "https://127.0.0.1")
        .replace("http://::1", "http://127.0.0.1")
        .replace("https://::1", "https://127.0.0.1");

    Some(url)
}

#[cfg(test)]
mod tests {
    use super::sniff_url;

    #[test]
    fn sniffs_loopback_url() {
        assert_eq!(
            sniff_url("ready on http://localhost:5173/"),
            Some("http://localhost:5173/".into())
        );
    }

    #[test]
    fn sniffs_bare_loopback_address() {
        assert_eq!(
            sniff_url("Local: localhost:3000"),
            Some("http://localhost:3000".into())
        );
    }

    #[test]
    fn normalizes_unspecified_hosts() {
        assert_eq!(
            sniff_url("Listening on http://0.0.0.0:8000"),
            Some("http://127.0.0.1:8000".into())
        );
        assert_eq!(
            sniff_url("Listening on http://[::1]:4000"),
            Some("http://127.0.0.1:4000".into())
        );
    }
}
