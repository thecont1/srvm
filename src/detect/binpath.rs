use std::{
    env,
    path::{Path, PathBuf},
};

pub fn look_path(tool: &str, root: &Path) -> Option<PathBuf> {
    bin_dirs(root)
        .into_iter()
        .find_map(|dir| executable_in(&dir, tool))
}

/// Resolve `tool` the way a spawned child would see it: `extra` directories
/// (fetched runtimes, shim dirs) take precedence, then the standard lookup
/// dirs for `root`. Returns an absolute, spawnable path — on Windows this is
/// usually a `.cmd`/`.exe`, since bare names cannot be exec'd by
/// `Command::new` without an extension.
pub fn resolve_for_spawn(tool: &str, root: &Path, extra: &[PathBuf]) -> Option<PathBuf> {
    extra
        .iter()
        .find_map(|dir| executable_in(dir, tool))
        .or_else(|| look_path(tool, root))
}

fn executable_in(dir: &Path, tool: &str) -> Option<PathBuf> {
    let direct = dir.join(tool);

    #[cfg(windows)]
    {
        // CreateProcess can only run extensions listed in PATHEXT; an
        // extensionless file (e.g. the `npm` sh script next to npm.cmd)
        // exists but is not spawnable, so it must not win over npm.cmd.
        if direct.extension().is_some() && is_executable(&direct) {
            return Some(direct);
        }
        let extensions = env::var_os("PATHEXT")
            .map(|v| {
                env::split_paths(&v)
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]);
        extensions.into_iter().find_map(|ext| {
            let candidate = dir.join(format!("{tool}{ext}"));
            is_executable(&candidate).then_some(candidate)
        })
    }

    #[cfg(not(windows))]
    {
        is_executable(&direct).then_some(direct)
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn bin_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(path) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&path));
    }

    if let Some(home) = home_dir() {
        dirs.extend([
            home.join(".bun/bin"),
            home.join(".deno/bin"),
            home.join(".local/share/pnpm"),
            home.join(".volta/bin"),
            home.join(".asdf/shims"),
            home.join(".local/share/mise/shims"),
            home.join(".pyenv/shims"),
            home.join(".rbenv/shims"),
        ]);

        #[cfg(windows)]
        {
            dirs.push(home.join(".bun/bin"));
            dirs.push(home.join(".deno/bin"));
            if let Some(local) = env::var_os("LOCALAPPDATA") {
                dirs.push(PathBuf::from(local).join("pnpm"));
            }
            if let Some(program_files) = env::var_os("ProgramFiles") {
                dirs.push(PathBuf::from(program_files).join("nodejs"));
            }
        }
    }

    dirs.push(root.join("node_modules/.bin"));
    dirs
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}
