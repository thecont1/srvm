use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{CommandSpec, PortInjection, ServeSpec, ToolResolver, probe};

pub fn rule_django(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !probe::file_exists(root, "manage.py") {
        return Ok(None);
    }

    Ok(
        py_plan(root, resolver, "python", ["manage.py", "runserver"]).map(|plan| {
            plan.into_spec(
                "django",
                Some(8000),
                PortInjection::Args(vec!["{port}".into()]),
            )
        }),
    )
}

pub fn rule_uvicorn(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !py_deps_contain(root, &["uvicorn", "fastapi"]) {
        return Ok(None);
    }

    let Some(module) = py_app_module(root, &["main", "app", "server", "asgi", "wsgi"]) else {
        return Ok(None);
    };

    Ok(py_plan(
        root,
        resolver,
        "uvicorn",
        [format!("{module}:app"), "--reload".into()],
    )
    .map(|plan| {
        plan.into_spec(
            "uvicorn",
            Some(8000),
            PortInjection::Args(vec!["--port".into(), "{port}".into()]),
        )
    }))
}

pub fn rule_flask(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !py_deps_contain(root, &["flask"]) {
        return Ok(None);
    }

    let Some(module) = py_app_module(root, &["app", "main", "server"]) else {
        return Ok(None);
    };

    Ok(py_plan(
        root,
        resolver,
        "flask",
        ["--app".into(), module, "run".into()],
    )
    .map(|plan| {
        plan.into_spec(
            "flask",
            Some(5000),
            PortInjection::Args(vec!["--port".into(), "{port}".into()]),
        )
    }))
}

/// A resolved Python command plus whatever bootstrap it needs first.
struct PyPlan {
    command: CommandSpec,
    installs: Vec<CommandSpec>,
    stamp: Option<crate::bootstrap::Stamp>,
}

impl PyPlan {
    fn into_spec(self, name: &str, url_hint: Option<u16>, port: PortInjection) -> ServeSpec {
        let mut spec = ServeSpec::new(
            name,
            py_tool_name(&self.command),
            self.command,
            url_hint,
            port,
        );
        for install in self.installs {
            spec = spec.with_install(install);
        }
        if let Some(stamp) = self.stamp {
            spec = spec.with_stamp(stamp);
        }
        spec
    }
}

/// Dependency manifests srvm turns into a virtualenv bootstrap.
const PY_SOURCES: &[&str] = &["requirements.txt", "requirements-dev.txt"];

/// Directories a Python project conventionally keeps its virtualenv in.
const VENV_DIRS: &[&str] = &[".venv", "venv", "env"];

/// Chooses the interpreter or tool `bin`, planning a virtualenv bootstrap when
/// the project declares dependencies but has no venv yet. A freshly cloned
/// Python repository rarely has one, and running a framework against a system
/// interpreter fails on the first import — so the venv is created rather than
/// reported. An existing venv is left alone unless its stamp says its declared
/// dependencies changed.
fn py_plan(
    root: &Path,
    resolver: &dyn ToolResolver,
    bin: &str,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Option<PyPlan> {
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let sources = dependency_sources(root);

    let mut installs = Vec::new();
    let mut stamp = None;
    let mut planned = None;

    if !sources.is_empty() {
        match venv_dir(root) {
            None => {
                if let Some(base) = base_interpreter(root, resolver) {
                    let dir = PathBuf::from(VENV_DIRS[0]);
                    installs.push(venv_create(&base, &dir));
                    installs.push(pip_install(&dir, &sources));
                    stamp = Some(crate::bootstrap::Stamp::python_venv(&dir, &sources));
                    planned = Some(dir);
                }
            }
            Some(dir) => {
                let candidate = crate::bootstrap::Stamp::python_venv(&dir, &sources);
                if matches!(
                    crate::bootstrap::state(root, &candidate),
                    crate::bootstrap::StampState::Stale | crate::bootstrap::StampState::Incomplete
                ) {
                    installs.push(pip_install(&dir, &sources));
                    stamp = Some(candidate);
                    planned = Some(dir);
                }
            }
        }
    }

    let (program, args) = py_program(root, resolver, bin, &args, planned.as_deref())?;
    Some(PyPlan {
        command: CommandSpec::new(program, args),
        installs,
        stamp,
    })
}

/// What to execute, in preference order: an existing venv, the venv this plan
/// is about to create or refresh, a lockfile manager wrapper, then the resolver
/// — which also covers a runtime srvm will fetch.
fn py_program(
    root: &Path,
    resolver: &dyn ToolResolver,
    bin: &str,
    args: &[String],
    planned: Option<&Path>,
) -> Option<(String, Vec<String>)> {
    if let Some(path) = venv_bin(root, bin) {
        return Some((lossy(&path), args.to_vec()));
    }
    if let Some(dir) = planned {
        return Some((lossy(&venv_bin_path(dir, bin)), args.to_vec()));
    }
    if bin == "python" {
        // "python3" always wins when resolvable (including via a fetchable
        // runtime). Only Windows falls back to a bare "python": there it is the
        // canonical launcher name, while on Unix it often means a legacy
        // python2 interpreter that would break modern frameworks.
        if resolver.resolve("python3", root).is_some() {
            return Some(("python3".into(), args.to_vec()));
        }
        #[cfg(windows)]
        if resolver.resolve("python", root).is_some() {
            return Some(("python".into(), args.to_vec()));
        }
        return None;
    }
    for (lockfile, manager) in [
        ("uv.lock", "uv"),
        ("poetry.lock", "poetry"),
        ("Pipfile", "pipenv"),
    ] {
        if probe::file_exists(root, lockfile) && resolver.resolve(manager, root).is_some() {
            let mut wrapped = vec!["run".to_string(), bin.to_string()];
            wrapped.extend(args.iter().cloned());
            return Some((manager.to_string(), wrapped));
        }
    }
    resolver
        .resolve(bin, root)
        .map(|_| (bin.to_string(), args.to_vec()))
}

fn dependency_sources(root: &Path) -> Vec<String> {
    PY_SOURCES
        .iter()
        .filter(|rel| probe::file_exists(root, rel))
        .map(|rel| (*rel).to_string())
        .collect()
}

fn venv_dir(root: &Path) -> Option<PathBuf> {
    VENV_DIRS
        .iter()
        .find(|dir| root.join(dir).is_dir())
        .map(PathBuf::from)
}

/// The interpreter a venv is created with. Never a venv: there is none.
fn base_interpreter(root: &Path, resolver: &dyn ToolResolver) -> Option<String> {
    if let Some(path) = resolver.resolve("python3", root) {
        return Some(lossy(&path));
    }
    #[cfg(windows)]
    if let Some(path) = resolver.resolve("python", root) {
        return Some(lossy(&path));
    }
    None
}

fn venv_create(base: &str, dir: &Path) -> CommandSpec {
    CommandSpec::new(base, ["-m".to_string(), "venv".to_string(), lossy(dir)])
}

/// Installs every declared source with the venv's own pip, so the app later
/// runs against exactly what was installed.
fn pip_install(dir: &Path, sources: &[String]) -> CommandSpec {
    let mut args = vec!["-m".to_string(), "pip".to_string(), "install".to_string()];
    for rel in sources {
        args.push("-r".to_string());
        args.push(rel.clone());
    }
    CommandSpec::new(lossy(&venv_bin_path(dir, "python")), args)
}

/// The path a tool has inside a venv, whether or not it exists yet.
fn venv_bin_path(dir: &Path, bin: &str) -> PathBuf {
    #[cfg(windows)]
    {
        dir.join("Scripts").join(format!("{bin}.exe"))
    }
    #[cfg(not(windows))]
    {
        dir.join("bin").join(bin)
    }
}

fn venv_bin(root: &Path, bin: &str) -> Option<PathBuf> {
    VENV_DIRS
        .iter()
        .map(PathBuf::from)
        .flat_map(|dir| venv_candidates(&dir, bin))
        .map(|rel| root.join(rel))
        .find(|candidate| candidate.is_file())
}

fn venv_candidates(dir: &Path, bin: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![
            dir.join("Scripts").join(format!("{bin}.exe")),
            dir.join("Scripts").join(format!("{bin}.bat")),
            dir.join("Scripts").join(bin),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![dir.join("bin").join(bin)]
    }
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn py_deps_contain(root: &Path, needles: &[&str]) -> bool {
    for rel in [
        "pyproject.toml",
        "requirements.txt",
        "requirements-dev.txt",
        "Pipfile",
    ] {
        if let Some(text) = probe::read_lossy(root, rel)
            && needles
                .iter()
                .any(|needle| text.to_lowercase().contains(needle))
        {
            return true;
        }
    }
    false
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

/// The tool name a user sees and `--select` matches: the executable's own name,
/// never a venv path.
fn py_tool_name(command: &CommandSpec) -> String {
    if command.program.contains("python") {
        return "python".into();
    }
    let name = Path::new(&command.program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&command.program);
    name.trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".bat")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use tempfile::tempdir;

    fn make_venv(root: &Path, dir: &str, bin: &str) {
        let path = root.join(venv_bin_path(Path::new(dir), bin));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn a_missing_venv_is_bootstrapped_before_the_app_runs() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "django\n").unwrap();

        let spec = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert_eq!(
            spec.installs
                .iter()
                .map(|install| install.command_line())
                .collect::<Vec<_>>(),
            vec![
                "python3 -m venv .venv".to_string(),
                format!(
                    "{} -m pip install -r requirements.txt",
                    venv_bin_path(Path::new(".venv"), "python").display()
                ),
            ]
        );
        assert_eq!(
            spec.command.program,
            venv_bin_path(Path::new(".venv"), "python").to_string_lossy()
        );
        assert_eq!(
            spec.stamp,
            Some(crate::bootstrap::Stamp::python_venv(
                Path::new(".venv"),
                &["requirements.txt".to_string()],
            ))
        );
    }

    #[test]
    fn an_existing_venv_without_a_stamp_is_left_alone() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "django\n").unwrap();
        make_venv(dir.path(), ".venv", "python");

        let spec = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert!(
            spec.installs.is_empty(),
            "an unstamped venv is unknown, not stale: {:?}",
            spec.installs
        );
        assert_eq!(spec.stamp, None);
        assert_eq!(
            spec.command.program,
            dir.path()
                .join(venv_bin_path(Path::new(".venv"), "python"))
                .to_string_lossy()
        );
    }

    #[test]
    fn a_venv_whose_install_never_finished_is_repaired() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "django\n").unwrap();
        make_venv(dir.path(), ".venv", "python");
        let stamp = crate::bootstrap::Stamp::python_venv(
            Path::new(".venv"),
            &["requirements.txt".to_string()],
        );
        crate::bootstrap::record_incomplete(dir.path(), &stamp).unwrap();

        let spec = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert_eq!(spec.installs.len(), 1, "{:?}", spec.installs);
        assert!(
            spec.installs[0]
                .command_line()
                .contains("pip install -r requirements.txt"),
            "{:?}",
            spec.installs
        );
    }

    #[test]
    fn a_stale_venv_stamp_reinstalls_only_the_dependencies() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "django\n").unwrap();
        make_venv(dir.path(), ".venv", "python");
        let stamp = crate::bootstrap::Stamp::python_venv(
            Path::new(".venv"),
            &["requirements.txt".to_string()],
        );
        crate::bootstrap::record(dir.path(), &stamp).unwrap();

        let fresh = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();
        assert!(fresh.installs.is_empty(), "{:?}", fresh.installs);

        std::fs::write(dir.path().join("requirements.txt"), "django\nflask\n").unwrap();
        let stale = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert_eq!(
            stale
                .installs
                .iter()
                .map(|install| install.command_line())
                .collect::<Vec<_>>(),
            vec![format!(
                "{} -m pip install -r requirements.txt",
                venv_bin_path(Path::new(".venv"), "python").display()
            )]
        );
        assert!(stale.stamp.is_some(), "a refresh is stamped again");
    }

    #[test]
    fn without_dependencies_there_is_no_bootstrap() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("manage.py"), "").unwrap();

        let spec = rule_django(dir.path(), &StubResolver::with(&["python3"]))
            .unwrap()
            .unwrap();

        assert!(spec.installs.is_empty(), "{:?}", spec.installs);
        assert_eq!(spec.command_line(), "python3 manage.py runserver");
    }

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
