use std::{io, net::TcpListener, sync::OnceLock};

use anyhow::Result;
use regex::Regex;

use crate::detect::{CommandSpec, PortInjection, ServeSpec};

static URL_PORT_RE: OnceLock<Regex> = OnceLock::new();

pub fn requested_port(
    spec: &ServeSpec,
    explicit: Option<u16>,
    inherited: Option<&str>,
) -> Result<Option<u16>> {
    if matches!(spec.port, PortInjection::None) {
        return Ok(None);
    }
    if let Some(port) = explicit {
        return Ok(Some(port));
    }
    if let PortInjection::Env(key) = &spec.port
        && let Some(raw) = inherited.map(str::trim).filter(|raw| !raw.is_empty())
    {
        return raw.parse::<u16>().map(Some).map_err(|_| {
            anyhow::anyhow!("{key}={raw:?} is not a valid port; expected a number 0-65535")
        });
    }
    Ok(spec.url_hint)
}

pub fn reserve(start: u16) -> io::Result<TcpListener> {
    bind_available_with(start, |port| TcpListener::bind(("127.0.0.1", port)))
}

pub fn apply(spec: &ServeSpec, port: u16) -> (CommandSpec, Vec<(String, String)>) {
    let mut command = spec.command.clone();
    let mut env = Vec::new();

    match &spec.port {
        PortInjection::Env(key) => env.push((key.clone(), port.to_string())),
        PortInjection::Args(template) => {
            let port = port.to_string();
            command
                .args
                .extend(template.iter().map(|arg| arg.replace("{port}", &port)));
        }
        PortInjection::None => {}
    }

    (command, env)
}

pub fn url_port(url: &str) -> Option<u16> {
    let caps = URL_PORT_RE
        .get_or_init(|| {
            Regex::new(
                r"(?i)^(https?)://(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|::1)(?::([0-9]+))?(?:[/?#]|$)",
            )
            .unwrap()
        })
        .captures(url)?;

    if let Some(port) = caps.get(3) {
        return port.as_str().parse::<u16>().ok().filter(|port| *port != 0);
    }

    Some(if caps[1].eq_ignore_ascii_case("https") {
        443
    } else {
        80
    })
}

fn bind_available_with<T>(start: u16, mut bind: impl FnMut(u16) -> io::Result<T>) -> io::Result<T> {
    if start == 0 {
        return bind(0);
    }
    for port in start..=start.saturating_add(100) {
        match bind(port) {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {}
            Err(err) => return Err(err),
        }
    }
    bind(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use std::{cell::RefCell, io::ErrorKind};
    use tempfile::tempdir;

    fn spec(port: PortInjection, hint: Option<u16>) -> ServeSpec {
        ServeSpec::new("app", "tool", CommandSpec::new("run", ["it"]), hint, port)
    }

    #[test]
    fn requested_prefers_explicit_then_inherited_then_hint() {
        let env_spec = spec(PortInjection::Env("PORT".into()), Some(8000));
        assert_eq!(
            requested_port(&env_spec, Some(5000), Some("9000")).unwrap(),
            Some(5000)
        );
        assert_eq!(
            requested_port(&env_spec, None, Some("9000")).unwrap(),
            Some(9000)
        );
        assert_eq!(
            requested_port(&env_spec, None, Some("  9000  ")).unwrap(),
            Some(9000)
        );
        assert_eq!(
            requested_port(&env_spec, None, Some("")).unwrap(),
            Some(8000)
        );
        assert_eq!(requested_port(&env_spec, None, None).unwrap(), Some(8000));
        assert_eq!(
            requested_port(&env_spec, Some(0), Some("9000")).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn requested_port_rejects_invalid_inherited() {
        let env_spec = spec(PortInjection::Env("PORT".into()), Some(8000));
        let err = requested_port(&env_spec, None, Some("abc")).unwrap_err();
        assert!(err.to_string().contains("PORT"));
    }

    #[test]
    fn requested_port_handles_unsupported_and_unknown() {
        let unsupported = spec(PortInjection::None, Some(8000));
        assert_eq!(
            requested_port(&unsupported, Some(5000), None).unwrap(),
            None
        );

        let opaque = spec(PortInjection::Env("PORT".into()), None);
        assert_eq!(requested_port(&opaque, None, None).unwrap(), None);
        assert_eq!(
            requested_port(&opaque, Some(5000), None).unwrap(),
            Some(5000)
        );
    }

    #[test]
    fn reserve_binds_another_port_when_start_is_occupied() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied = held.local_addr().unwrap().port();

        let listener = reserve(occupied).unwrap();
        let selected = listener.local_addr().unwrap().port();
        assert_ne!(selected, occupied);
        assert_ne!(selected, 0);
    }

    #[test]
    fn bind_available_with_returns_first_success() {
        let attempts = RefCell::new(Vec::new());
        let bound = bind_available_with(5000, |port| {
            attempts.borrow_mut().push(port);
            Ok(port)
        })
        .unwrap();

        assert_eq!(bound, 5000);
        assert_eq!(*attempts.borrow(), vec![5000]);
    }

    #[test]
    fn bind_available_with_skips_busy_candidates() {
        let attempts = RefCell::new(Vec::new());
        let bound = bind_available_with(5000, |port| {
            attempts.borrow_mut().push(port);
            if port == 5002 {
                Ok(port)
            } else {
                Err(io::Error::new(ErrorKind::AddrInUse, "busy"))
            }
        })
        .unwrap();

        assert_eq!(bound, 5002);
        assert_eq!(*attempts.borrow(), vec![5000, 5001, 5002]);
    }

    #[test]
    fn reserve_zero_picks_os_assigned_nonzero_port() {
        let listener = reserve(0).unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }

    #[test]
    fn reservation_holds_until_dropped() {
        let listener = reserve(0).unwrap();
        let port = listener.local_addr().unwrap().port();

        let held = TcpListener::bind(("127.0.0.1", port));
        assert!(held.is_err());

        drop(listener);
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn bind_available_with_falls_back_to_zero_after_101_busy() {
        let attempts = RefCell::new(Vec::new());
        let bound = bind_available_with(5000, |port| {
            attempts.borrow_mut().push(port);
            if port == 0 {
                Ok(port)
            } else {
                Err(io::Error::new(ErrorKind::AddrInUse, "busy"))
            }
        })
        .unwrap();

        assert_eq!(bound, 0);
        let attempts = attempts.borrow();
        assert_eq!(attempts.len(), 102);
        assert_eq!(attempts[0], 5000);
        assert_eq!(attempts[100], 5100);
        assert_eq!(attempts[101], 0);
    }

    #[test]
    fn bind_available_with_does_not_wrap_past_65535() {
        let attempts = RefCell::new(Vec::new());
        let bound = bind_available_with(65535, |port| {
            attempts.borrow_mut().push(port);
            if port == 0 {
                Ok(port)
            } else {
                Err(io::Error::new(ErrorKind::AddrInUse, "busy"))
            }
        })
        .unwrap();

        assert_eq!(bound, 0);
        assert_eq!(*attempts.borrow(), vec![65535, 0]);
    }

    #[test]
    fn bind_available_with_aborts_non_busy_errors() {
        let attempts = RefCell::new(Vec::new());
        let err = bind_available_with(5000, |port| {
            attempts.borrow_mut().push(port);
            Err::<u16, _>(io::Error::new(ErrorKind::PermissionDenied, "denied"))
        })
        .unwrap_err();

        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
        assert_eq!(*attempts.borrow(), vec![5000]);
    }

    #[test]
    fn bind_available_with_propagates_fallback_error() {
        let err = bind_available_with(5000, |port| {
            if port == 0 {
                Err::<u16, _>(io::Error::new(ErrorKind::AddrNotAvailable, "gone"))
            } else {
                Err(io::Error::new(ErrorKind::AddrInUse, "busy"))
            }
        })
        .unwrap_err();

        assert_eq!(err.kind(), ErrorKind::AddrNotAvailable);
    }

    #[test]
    fn apply_env_injects_single_env_pair() {
        let spec = spec(PortInjection::Env("PORT".into()), None);
        let (command, env) = apply(&spec, 8123);

        assert_eq!(command.command_line(), "run it");
        assert_eq!(env, vec![("PORT".to_string(), "8123".to_string())]);
    }

    #[test]
    fn apply_args_replaces_port_placeholder_verbatim() {
        let spec = spec(
            PortInjection::Args(vec!["-a".into(), "127.0.0.1:{port}".into()]),
            None,
        );
        let original = spec.command.clone();

        let (command, env) = apply(&spec, 8123);

        assert_eq!(command.command_line(), "run it -a 127.0.0.1:8123");
        assert!(env.is_empty());
        assert_eq!(spec.command, original);
    }

    #[test]
    fn apply_none_leaves_command_untouched() {
        let spec = spec(PortInjection::None, None);
        let (command, env) = apply(&spec, 8123);

        assert_eq!(command.command_line(), "run it");
        assert!(env.is_empty());
    }

    #[test]
    fn apply_renders_framework_templates_exactly() {
        let rendered = |files: &[(&str, &str)], tools: &[&str], name: &str| {
            let dir = tempdir().unwrap();
            for (rel, content) in files {
                let path = dir.path().join(rel);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, content).unwrap();
            }
            let specs = crate::detect::detect_with(dir.path(), &StubResolver::with(tools)).unwrap();
            let spec = specs.iter().find(|spec| spec.name == name).unwrap();
            apply(spec, 8123).0.command_line()
        };

        assert_eq!(
            rendered(&[("manage.py", "")], &["python3"], "django"),
            "python3 manage.py runserver 8123"
        );
        assert_eq!(
            rendered(
                &[("mkdocs.yml", "site_name: docs\n")],
                &["mkdocs"],
                "mkdocs"
            ),
            "mkdocs serve -a 127.0.0.1:8123"
        );
        assert_eq!(
            rendered(
                &[("composer.json", "{}"), ("artisan", "#!/usr/bin/env php\n")],
                &["php"],
                "laravel"
            ),
            "php artisan serve --port=8123"
        );
        assert_eq!(
            rendered(&[("hugo.toml", "baseURL='x'\n")], &["hugo"], "hugo"),
            "hugo server --port 8123"
        );
        assert_eq!(
            rendered(&[("bin/rails", "#!/usr/bin/env ruby\n")], &[], "rails"),
            "bin/rails server -p 8123"
        );
        assert_eq!(
            rendered(
                &[("wrangler.toml", "name='w'\n")],
                &["wrangler"],
                "wrangler"
            ),
            "wrangler dev --port 8123"
        );
    }

    #[test]
    fn url_port_parses_loopback_authority() {
        assert_eq!(url_port("http://localhost:3000"), Some(3000));
        assert_eq!(url_port("http://127.0.0.1:8080/path?q=1#f"), Some(8080));
        assert_eq!(url_port("http://[::1]:4000"), Some(4000));
        assert_eq!(url_port("http://0.0.0.0:8000"), Some(8000));
        assert_eq!(url_port("HTTP://LOCALHOST:3000/"), Some(3000));
        assert_eq!(url_port("http://127.0.0.1"), Some(80));
        assert_eq!(url_port("https://localhost"), Some(443));
        assert_eq!(url_port("https://localhost/"), Some(443));
    }

    #[test]
    fn url_port_rejects_non_loopback_and_bad_ports() {
        assert_eq!(url_port("http://example.com:3000"), None);
        assert_eq!(url_port("http://localhost:0"), None);
        assert_eq!(url_port("http://localhost:65536"), None);
        assert_eq!(url_port("http://127.0.0.1:"), None);
        assert_eq!(url_port("http://127.0.0.10:80"), None);
        assert_eq!(url_port("not a url"), None);
        assert_eq!(url_port("ftp://localhost:21"), None);
    }
}
