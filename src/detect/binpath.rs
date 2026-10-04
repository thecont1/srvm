use std::{
    env,
    path::{Path, PathBuf},
};

pub fn look_path(tool: &str, root: &Path) -> Option<PathBuf> {
    bin_dirs(root)
        .into_iter()
        .find_map(|dir| executable_in(&dir, tool))
}

fn executable_in(dir: &Path, tool: &str) -> Option<PathBuf> {
    let direct = dir.join(tool);
    if is_executable(&direct) {
        return Some(direct);
    }

    #[cfg(windows)]
    {
        let extensions = env::var_os("PATHEXT")
            .map(|v| {
                env::split_paths(&v)
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]);
        for ext in extensions {
            let candidate = dir.join(format!("{tool}{ext}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
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
