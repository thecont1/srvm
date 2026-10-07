use std::path::{Path, PathBuf};
use std::{ffi::OsStr, fs};

use anyhow::Result;

use crate::detect::{self, CommandSpec, ServeSpec, ToolResolver};

/// Hard cap on visited candidate roots. Hitting it is reported, never silent.
pub const MAX_CANDIDATE_ROOTS: usize = 128;

/// Conventional direct children of the selected root that hold an app.
pub const APP_DIRS: &[&str] = &["frontend", "backend", "client", "server", "web", "api"];

/// Conventional parents whose immediate children hold an app.
pub const APP_PARENTS: &[&str] = &["apps", "packages", "services"];

/// Conventional asset or build-output directories, tried only when nothing
/// else matched — a served dist/ beats "nothing to run".
pub const ASSET_DIRS: &[&str] = &["public", "www", "site", "dist", "build", "out"];

/// Directories never descended into: generated output and dependency trees.
pub const GENERATED_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "env",
    "deps",
    "__pycache__",
];

/// One detected app, located inside the workspace.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Canonicalized directory the app runs from.
    pub root: PathBuf,
    /// App root relative to the workspace root; empty for the root itself.
    pub rel: PathBuf,
    pub spec: ServeSpec,
}

impl Candidate {
    /// Qualified id used by `--select`: `<rel>:<spec name>`, or the bare name
    /// for a root candidate.
    pub fn id(&self) -> String {
        if self.rel.as_os_str().is_empty() {
            self.spec.name.clone()
        } else {
            format!("{}:{}", self.rel.to_string_lossy(), self.spec.name)
        }
    }
}

/// The bounded discovery result for one workspace.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub candidates: Vec<Candidate>,
    /// Diagnostics: cap truncation, unreadable child markers.
    pub notes: Vec<String>,
}

pub fn discover(root: &Path) -> Result<Workspace> {
    let root = root.canonicalize()?;
    discover_with(&root, &detect::WorkspaceResolver::new(&root))
}

pub fn discover_with(root: &Path, resolver: &dyn ToolResolver) -> Result<Workspace> {
    let root = root.canonicalize()?;
    let mut notes = Vec::new();
    let mut roots = candidate_roots(&root, &mut notes)?;

    if roots.len() > MAX_CANDIDATE_ROOTS {
        notes.push(format!(
            "found {} candidate roots; searching the first {MAX_CANDIDATE_ROOTS} only",
            roots.len()
        ));
        roots.truncate(MAX_CANDIDATE_ROOTS);
    }

    let mut candidates = Vec::new();
    for (index, dir) in roots.iter().enumerate() {
        let app_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let rel = relative_to(&root, &app_root);
        match detect::detect_with(&app_root, resolver) {
            Ok(specs) => candidates.extend(specs.into_iter().map(|spec| Candidate {
                root: app_root.clone(),
                rel: rel.clone(),
                spec,
            })),
            // The selected root keeps its existing behavior; a child marker
            // that cannot be read is a path-qualified diagnostic instead of a
            // failed launch.
            Err(err) if index == 0 => return Err(err),
            Err(err) => notes.push(format!("skipping {}: {err}", rel.display())),
        }
    }

    if candidates.is_empty() {
        candidates.extend(static_fallback(&root));
    }

    Ok(Workspace {
        root,
        candidates,
        notes,
    })
}

fn candidate_roots(root: &Path, notes: &mut Vec<String>) -> Result<Vec<PathBuf>> {
    let mut roots = vec![root.to_path_buf()];

    for name in APP_DIRS {
        let dir = root.join(name);
        if is_real_dir(&dir) {
            push_unique(&mut roots, dir);
        }
    }

    for parent in APP_PARENTS {
        let base = root.join(parent);
        if !is_real_dir(&base) {
            continue;
        }
        let mut children = Vec::new();
        match fs::read_dir(&base) {
            Ok(entries) => {
                for entry in entries {
                    let Ok(entry) = entry else { continue };
                    // file_type() is the symlink-avoiding metadata call, so a
                    // linked directory is never descended into.
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    if !file_type.is_dir() || !is_candidate_name(&entry.file_name()) {
                        continue;
                    }
                    children.push(entry.path());
                }
            }
            Err(err) => {
                notes.push(format!("could not read {parent}: {err}"));
                continue;
            }
        }
        children.sort();
        for child in children {
            push_unique(&mut roots, child);
        }
    }

    Ok(roots)
}

fn static_fallback(root: &Path) -> Vec<Candidate> {
    for name in ASSET_DIRS {
        let dir = root.join(name);
        if is_real_dir(&dir) && dir.join("index.html").is_file() {
            let app_root = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let rel = relative_to(root, &app_root);
            return vec![Candidate {
                root: app_root,
                rel,
                spec: ServeSpec::static_site(CommandSpec::new("srvm", ["static", "."]), 8000),
            }];
        }
    }
    Vec::new()
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_dir())
        .unwrap_or(false)
}

fn is_candidate_name(name: &OsStr) -> bool {
    match name.to_str() {
        Some(name) => !name.starts_with('.') && !GENERATED_DIRS.contains(&name),
        None => true,
    }
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map_or_else(|_| PathBuf::new(), |rel| rel.to_path_buf())
}

fn push_unique(roots: &mut Vec<PathBuf>, dir: PathBuf) {
    if !roots.contains(&dir) {
        roots.push(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use std::fs;
    use tempfile::tempdir;

    const PKG: &str = r#"{"scripts":{"dev":"node server.js"}}"#;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn rels(workspace: &Workspace) -> Vec<String> {
        workspace
            .candidates
            .iter()
            .map(|candidate| candidate.rel.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn discovered(files: &[(&str, &str)], tools: &[&str]) -> Workspace {
        let dir = tempdir().unwrap();
        for (rel, body) in files {
            write(dir.path(), rel, body);
        }
        discover_with(dir.path(), &StubResolver::with(tools)).unwrap()
    }

    #[test]
    fn discovers_conventional_directories() {
        let workspace = discovered(
            &[("frontend/package.json", PKG), ("backend/manage.py", "")],
            &["npm", "python3"],
        );

        assert_eq!(rels(&workspace), vec!["frontend", "backend"]);
        assert_eq!(workspace.candidates[0].spec.name, "package:dev");
        assert_eq!(workspace.candidates[1].spec.name, "django");
        assert_eq!(workspace.candidates[0].id(), "frontend:package:dev");
    }

    #[test]
    fn orders_sub_roots_by_name_within_each_parent() {
        let workspace = discovered(
            &[
                ("apps/web/package.json", PKG),
                ("apps/admin/package.json", PKG),
                ("packages/ui/package.json", PKG),
            ],
            &["npm"],
        );

        assert_eq!(
            rels(&workspace),
            vec!["apps/admin", "apps/web", "packages/ui"]
        );
    }

    #[test]
    fn root_candidates_come_first() {
        let workspace = discovered(
            &[("package.json", PKG), ("frontend/package.json", PKG)],
            &["npm"],
        );

        assert_eq!(rels(&workspace), vec!["", "frontend"]);
        assert_eq!(workspace.candidates[0].id(), "package:dev");
    }

    #[test]
    fn skips_hidden_and_generated_directories() {
        let workspace = discovered(
            &[
                ("apps/.hidden/package.json", PKG),
                ("apps/node_modules/pkg/package.json", PKG),
                ("apps/dist/package.json", PKG),
                ("apps/web/package.json", PKG),
            ],
            &["npm"],
        );

        assert_eq!(rels(&workspace), vec!["apps/web"]);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinked_app_directories() {
        let dir = tempdir().unwrap();
        write(dir.path(), "apps/real/package.json", PKG);
        std::os::unix::fs::symlink(dir.path().join("apps/real"), dir.path().join("apps/link"))
            .unwrap();

        let workspace = discover_with(dir.path(), &StubResolver::with(&["npm"])).unwrap();

        assert_eq!(rels(&workspace), vec!["apps/real"]);
    }

    #[test]
    fn caps_candidate_roots_with_a_diagnostic() {
        let dir = tempdir().unwrap();
        write(dir.path(), "package.json", PKG);
        for index in 0..MAX_CANDIDATE_ROOTS + 2 {
            write(dir.path(), &format!("apps/app{index:03}/package.json"), PKG);
        }

        let workspace = discover_with(dir.path(), &StubResolver::with(&["npm"])).unwrap();

        assert_eq!(workspace.candidates.len(), MAX_CANDIDATE_ROOTS);
        assert!(
            workspace.notes.iter().any(|note| note.contains("128")),
            "cap must be reported: {:?}",
            workspace.notes
        );
    }

    #[test]
    fn malformed_child_marker_warns_without_aborting() {
        let workspace = discovered(
            &[("package.json", PKG), ("frontend/package.json", "{")],
            &["npm"],
        );

        assert_eq!(rels(&workspace), vec![""]);
        assert!(
            workspace
                .notes
                .iter()
                .any(|note| note.contains("frontend") && note.contains("package.json")),
            "{:?}",
            workspace.notes
        );
    }

    #[test]
    fn malformed_root_marker_still_errors() {
        let dir = tempdir().unwrap();
        write(dir.path(), "package.json", "{");

        assert!(discover_with(dir.path(), &StubResolver::with(&["npm"])).is_err());
    }

    #[test]
    fn static_fallback_only_when_nothing_else_matches() {
        let alone = discovered(&[("public/index.html", "hello")], &[]);
        assert_eq!(rels(&alone), vec!["public"]);
        assert!(alone.candidates[0].spec.is_static);

        let mixed = discovered(
            &[
                ("frontend/package.json", PKG),
                ("public/index.html", "hello"),
            ],
            &["npm"],
        );
        assert_eq!(rels(&mixed), vec!["frontend"]);
    }

    #[test]
    fn serves_build_output_when_nothing_else_matches() {
        // A repo whose only servable artifact is build output: marker-free
        // root, tooling in a non-conventional dir, dist/index.html present.
        // The fallback must serve it rather than report nothing to run.
        for dir in ["dist", "build", "out"] {
            let ws = discovered(
                &[
                    ("deck.yaml", "slides: []\n"),
                    ("engine/build.py", ""),
                    (&format!("{dir}/index.html"), "hello"),
                ],
                &[],
            );
            assert_eq!(rels(&ws), vec![dir]);
            assert!(ws.candidates[0].spec.is_static);
        }
    }

    #[test]
    fn candidate_roots_are_canonical() {
        let dir = tempdir().unwrap();
        write(dir.path(), "frontend/package.json", PKG);
        let canonical = dir.path().canonicalize().unwrap();

        let workspace = discover_with(dir.path(), &StubResolver::with(&["npm"])).unwrap();

        assert_eq!(workspace.root, canonical);
        assert_eq!(workspace.candidates[0].root, canonical.join("frontend"));
    }

    #[test]
    fn empty_workspace_yields_no_candidates() {
        let dir = tempdir().unwrap();

        let workspace = discover_with(dir.path(), &StubResolver::with(&["npm"])).unwrap();

        assert!(workspace.candidates.is_empty());
        assert!(workspace.notes.is_empty());
    }
}
