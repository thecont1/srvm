use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{CommandSpec, PortInjection, ServeSpec, ToolResolver, probe};

pub fn rule_django(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !probe::file_exists(root, "manage.py") {
        return Ok(None);
    }

    Ok(
        py_command(root, resolver, "python", ["manage.py", "runserver"]).map(|command| {
            ServeSpec::new(
                "django",
                py_tool_name(&command),
                command,
                Some(8000),
                PortInjection::Args(vec!["{port}".into()]),
            )
        }),
    )
}

pub fn rule_uvicorn(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !py_deps_contain(root, &["uvicorn", "fastapi"])? {
        return Ok(None);
    }

    let Some(module) = py_app_module(root, &["main", "app", "server", "asgi", "wsgi"]) else {
        return Ok(None);
    };

    Ok(py_command(
        root,
        resolver,
        "uvicorn",
        [format!("{module}:app"), "--reload".into()],
    )
    .map(|command| {
        ServeSpec::new(
            "uvicorn",
            py_tool_name(&command),
            command,
            Some(8000),
            PortInjection::Args(vec!["--port".into(), "{port}".into()]),
        )
    }))
}

pub fn rule_flask(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !py_deps_contain(root, &["flask"])? {
        return Ok(None);
    }

    let Some(module) = py_app_module(root, &["app", "main", "server"]) else {
        return Ok(None);
    };

    Ok(py_command(
        root,
        resolver,
        "flask",
        ["--app".into(), module, "run".into()],
    )
    .map(|command| {
        ServeSpec::new(
            "flask",
            py_tool_name(&command),
            command,
            Some(5000),
            PortInjection::Args(vec!["--port".into(), "{port}".into()]),
        )
    }))
}

fn py_command(
    root: &Path,
    resolver: &dyn ToolResolver,
    bin: &str,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Option<CommandSpec> {
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();

    if bin == "python" {
        if let Some(path) = venv_bin(root, "python") {
            return Some(CommandSpec::new(path.to_string_lossy(), args));
        }
        for candidate in ["python3", "python"] {
            if resolver.resolve(candidate, root).is_some() {
                return Some(CommandSpec::new(candidate, args));
            }
        }
        return None;
    }

    if let Some(path) = venv_bin(root, bin) {
        return Some(CommandSpec::new(path.to_string_lossy(), args));
    }

    if probe::file_exists(root, "uv.lock") && resolver.resolve("uv", root).is_some() {
        let mut wrapper_args = vec!["run".to_string(), bin.to_string()];
        wrapper_args.extend(args);
        return Some(CommandSpec::new("uv", wrapper_args));
    }

    if probe::file_exists(root, "poetry.lock") && resolver.resolve("poetry", root).is_some() {
        let mut wrapper_args = vec!["run".to_string(), bin.to_string()];
        wrapper_args.extend(args);
        return Some(CommandSpec::new("poetry", wrapper_args));
    }

    if probe::file_exists(root, "Pipfile") && resolver.resolve("pipenv", root).is_some() {
        let mut wrapper_args = vec!["run".to_string(), bin.to_string()];
        wrapper_args.extend(args);
        return Some(CommandSpec::new("pipenv", wrapper_args));
    }

    resolver
        .resolve(bin, root)
        .map(|_| CommandSpec::new(bin, args))
}

fn venv_bin(root: &Path, bin: &str) -> Option<PathBuf> {
    for dir in [".venv", "venv", "env"] {
        #[cfg(windows)]
        let candidates = [
            root.join(dir).join("Scripts").join(format!("{bin}.exe")),
            root.join(dir).join("Scripts").join(format!("{bin}.bat")),
            root.join(dir).join("Scripts").join(bin),
        ];

        #[cfg(not(windows))]
        let candidates = [root.join(dir).join("bin").join(bin)];

        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn py_deps_contain(root: &Path, needles: &[&str]) -> Result<bool> {
    for rel in [
        "pyproject.toml",
        "requirements.txt",
        "requirements-dev.txt",
        "Pipfile",
    ] {
        if probe::file_exists(root, rel) {
            let text = probe::read_to_string(root, rel)?.to_lowercase();
            if needles.iter().any(|needle| text.contains(needle)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn py_app_module(root: &Path, modules: &[&str]) -> Option<String> {
    for module in modules {
        let rel = format!("{module}.py");
        if probe::file_exists(root, &rel) {
            return Some((*module).to_string());
        }
    }
    None
}

fn py_tool_name(command: &CommandSpec) -> String {
    if command.program.contains("python") {
        "python".into()
    } else {
        command.program.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use tempfile::tempdir;

    #[test]
    fn detects_django_manage_py() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();

        let spec = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert_eq!(spec.command_line(), "python3 manage.py runserver");
    }

    #[test]
    fn detects_uvicorn_app_module() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "fastapi = '*'\n").unwrap();
        std::fs::write(dir.path().join("main.py"), "app = FastAPI()\n").unwrap();

        let spec = rule_uvicorn(dir.path(), &StubResolver::with(&["uvicorn"]))
            .unwrap()
            .unwrap();

        assert_eq!(spec.command_line(), "uvicorn main:app --reload");
    }
}
