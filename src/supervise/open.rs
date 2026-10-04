use anyhow::Result;
use std::{
    env,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserCommand {
    program: String,
    args: Vec<String>,
}

pub fn open_browser(url: &str) -> Result<()> {
    let wsl = is_wsl();
    let candidates = browser_candidates(url, env::var("BROWSER").ok(), wsl);
    if wsl && !candidates.is_empty() {
        thread::sleep(Duration::from_millis(250));
    }

    for candidate in candidates {
        let mut command = Command::new(&candidate.program);
        command
            .args(&candidate.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if command.spawn().is_ok() {
            return Ok(());
        }
    }

    Ok(())
}

fn browser_candidates(url: &str, browser: Option<String>, wsl: bool) -> Vec<BrowserCommand> {
    let mut candidates = Vec::new();

    if let Some(browser) = browser
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if browser.eq_ignore_ascii_case("none") {
            return candidates;
        }
        candidates.push(browser_env_command(browser, url));
    }

    candidates.extend(platform_candidates(url, wsl));
    candidates
}

fn browser_env_command(browser: &str, url: &str) -> BrowserCommand {
    let mut parts = browser
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let program = parts.first().cloned().unwrap_or_default();
    let mut args = parts.drain(1..).collect::<Vec<_>>();

    if args.iter().any(|arg| arg.contains("%s")) {
        for arg in &mut args {
            *arg = arg.replace("%s", url);
        }
    } else {
        args.push(url.to_string());
    }

    BrowserCommand { program, args }
}

#[cfg(target_os = "macos")]
fn platform_candidates(url: &str, _wsl: bool) -> Vec<BrowserCommand> {
    vec![BrowserCommand {
        program: "open".into(),
        args: vec![url.into()],
    }]
}

#[cfg(target_os = "windows")]
fn platform_candidates(url: &str, _wsl: bool) -> Vec<BrowserCommand> {
    vec![
        BrowserCommand {
            program: "rundll32".into(),
            args: vec!["url.dll,FileProtocolHandler".into(), url.into()],
        },
        BrowserCommand {
            program: "cmd".into(),
            args: vec!["/C".into(), "start".into(), "".into(), url.into()],
        },
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_candidates(url: &str, wsl: bool) -> Vec<BrowserCommand> {
    let mut candidates = Vec::new();

    if wsl {
        candidates.push(BrowserCommand {
            program: "wslview".into(),
            args: vec![url.into()],
        });
        candidates.push(BrowserCommand {
            program: "cmd.exe".into(),
            args: vec!["/C".into(), "start".into(), "".into(), url.into()],
        });
    }

    candidates.extend([
        BrowserCommand {
            program: "xdg-open".into(),
            args: vec![url.into()],
        },
        BrowserCommand {
            program: "sensible-browser".into(),
            args: vec![url.into()],
        },
        BrowserCommand {
            program: "gio".into(),
            args: vec!["open".into(), url.into()],
        },
        BrowserCommand {
            program: "google-chrome".into(),
            args: vec![url.into()],
        },
        BrowserCommand {
            program: "chromium".into(),
            args: vec![url.into()],
        },
    ]);

    candidates
}

fn is_wsl() -> bool {
    env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/version")
            .map(|version| version.to_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_none_disables_opening() {
        assert!(browser_candidates("http://localhost:3000", Some("none".into()), false).is_empty());
    }

    #[test]
    fn browser_env_is_used_before_platform_fallbacks() {
        let candidates = browser_candidates(
            "http://localhost:3000",
            Some("firefox --new-tab %s".into()),
            false,
        );

        assert_eq!(candidates[0].program, "firefox");
        assert_eq!(candidates[0].args, ["--new-tab", "http://localhost:3000"]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_candidates_include_fallback_chain() {
        let candidates = browser_candidates("http://localhost:3000", None, false);
        let programs = candidates
            .iter()
            .map(|candidate| candidate.program.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            programs,
            [
                "xdg-open",
                "sensible-browser",
                "gio",
                "google-chrome",
                "chromium"
            ]
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn wsl_candidates_precede_linux_fallbacks() {
        let candidates = browser_candidates("http://localhost:3000", None, true);

        assert_eq!(candidates[0].program, "wslview");
        assert_eq!(candidates[1].program, "cmd.exe");
    }
}
