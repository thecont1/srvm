use std::{
    env,
    path::{Path, PathBuf},
};

pub fn look_path(tool: &str, root: &Path) -> Option<PathBuf> {
    look_path_within(tool, root, None)
}

/// `look_path`, with `node_modules/.bin` lookup walking from `root` up to —
/// never past — `ceiling`. Without a ceiling only `root`'s own
/// `node_modules/.bin` is searched, which keeps detection inside the selected
/// root.
pub fn look_path_within(tool: &str, root: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
    bin_dirs(root, ceiling)
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
        .map(|path| std::path::absolute(&path).unwrap_or(path))
}

fn executable_in(dir: &Path, tool: &str) -> Option<PathBuf> {
    let direct = dir.join(tool);

    #[cfg(windows)]
    {
        // CreateProcess can only launch a fixed set of extensions; an
        // extensionless file (e.g. the `npm` sh script next to npm.cmd)
        // exists but is not spawnable, and PATHEXT may list non-launchable
        // entries like .PS1. Restrict both paths to the launchable set.
        if launchable(&direct) && is_executable(&direct) {
            return Some(direct);
        }
        let extensions = env::var_os("PATHEXT")
            .map(|v| {
                env::split_paths(&v)
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]);
        extensions
            .into_iter()
            .filter(|ext| launchable(Path::new(&format!("x{ext}"))))
            .find_map(|ext| {
                let candidate = dir.join(format!("{tool}{ext}"));
                is_executable(&candidate).then_some(candidate)
            })
    }

    #[cfg(not(windows))]
    {
        is_executable(&direct).then_some(direct)
    }
}

/// Extensions CreateProcess can launch directly (BAT/CMD get the cmd.exe
/// dispatch; everything else — .PS1, .TXT, extensionless — is unspawnable).
#[cfg(windows)]
fn launchable(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "com" | "exe" | "bat" | "cmd"
            )
        })
        .unwrap_or(false)
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

fn bin_dirs(root: &Path, ceiling: Option<&Path>) -> Vec<PathBuf> {
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

    dirs.extend(node_modules_bins(root, ceiling));
    dirs
}

/// `node_modules/.bin` for the app root and, when a workspace ceiling is
/// given, each ancestor up to and including it — hoisted dependencies are
/// visible, nothing above the workspace is.
fn node_modules_bins(root: &Path, ceiling: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = Some(root);

    while let Some(dir) = current {
        dirs.push(dir.join("node_modules/.bin"));
        if ceiling.is_none() || ceiling == Some(dir) {
            break;
        }
        current = dir.parent();
    }

    dirs
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}
