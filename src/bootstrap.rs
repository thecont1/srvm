//! Universal bootstrap installs and their stamps.
//!
//! A cloned repository should run without reading a contributing guide, so srvm
//! performs the conventional, untracked setup an ecosystem expects — a
//! virtualenv for Python, `node_modules` for JavaScript. Everything here is a
//! read except [`record`], and writes go only into the untracked directory a
//! stamp describes.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub const STAMP_FILE: &str = ".srvm-bootstrap";
pub const STAMP_VERSION: &str = "srvm-bootstrap-v1";

/// One versioned, tab-separated line written into the untracked directory it
/// describes:
///
/// `srvm-bootstrap-v1<TAB><purpose><TAB><source-rel-path><TAB>sha256:<hex>`
///
/// Multiple sources are listed comma-separated and digested as a sorted list of
/// `<rel>\0<sha256>\n` records, so any content change in any source changes the
/// digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub purpose: String,
    /// Directory the stamp lives in, relative to the app root.
    pub dir: PathBuf,
    /// Source files, relative to the app root.
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampState {
    /// No stamp at all: unknown, do not reinstall.
    Missing,
    /// An install was started here and never finished: repair it. A stamp is
    /// marked incomplete before the install steps run and replaced only when
    /// they all succeed, so a crash mid-install cannot leave a directory that
    /// looks complete.
    Incomplete,
    /// A stamp that cannot be read or understood: unknown, do not reinstall.
    Unknown,
    /// The recorded digest matches the sources on disk.
    Fresh,
    /// The recorded digest differs: the sources changed, reinstall.
    Stale,
}

impl Stamp {
    /// A stamp for the virtualenv `dir` (relative to the app root) covering the
    /// Python dependency sources that were installed into it.
    pub fn python_venv(dir: &Path, sources: &[String]) -> Self {
        Self {
            purpose: "python-requirements".into(),
            dir: dir.to_path_buf(),
            sources: sources.to_vec(),
        }
    }
}

pub fn state(root: &Path, stamp: &Stamp) -> StampState {
    let path = root.join(&stamp.dir).join(STAMP_FILE);
    let Ok(text) = fs::read_to_string(&path) else {
        return StampState::Missing;
    };
    if text
        .lines()
        .next()
        .is_some_and(|line| line.ends_with(INCOMPLETE_DIGEST))
    {
        return StampState::Incomplete;
    }
    let Some(recorded) = parse(&text) else {
        return StampState::Unknown;
    };
    let (purpose, _, digest) = recorded;
    if purpose != stamp.purpose {
        return StampState::Unknown;
    }
    match sources_digest(root, &stamp.sources) {
        Ok(expected) if expected == digest => StampState::Fresh,
        Ok(_) => StampState::Stale,
        // A source that cannot be read is unknown, not a reason to reinstall.
        Err(_) => StampState::Unknown,
    }
}

pub fn record(root: &Path, stamp: &Stamp) -> Result<()> {
    let dir = root.join(&stamp.dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let digest = sources_digest(root, &stamp.sources)?;
    let path = dir.join(STAMP_FILE);
    fs::write(&path, format_stamp(stamp, &digest))
        .with_context(|| format!("writing {}", path.display()))
}

/// Records that an install into this stamp's directory has started but not
/// finished, so an interrupted bootstrap is repaired on the next run instead
/// of being mistaken for a completed one.
pub fn record_incomplete(root: &Path, stamp: &Stamp) -> Result<()> {
    let dir = root.join(&stamp.dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(STAMP_FILE);
    fs::write(&path, format_stamp(stamp, INCOMPLETE_DIGEST))
        .with_context(|| format!("writing {}", path.display()))
}

/// The digest placeholder written before the install steps run.
pub const INCOMPLETE_DIGEST: &str = "incomplete";

pub fn format_stamp(stamp: &Stamp, digest: &str) -> String {
    format!(
        "{STAMP_VERSION}\t{}\t{}\t{digest}\n",
        stamp.purpose,
        stamp.sources.join(",")
    )
}

pub fn parse(text: &str) -> Option<(String, Vec<String>, String)> {
    let line = text.lines().next()?;
    let mut fields = line.split('\t');
    if fields.next()? != STAMP_VERSION {
        return None;
    }
    let purpose = fields.next()?.to_string();
    let sources = fields
        .next()?
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let digest = fields.next()?.to_string();
    if fields.next().is_some() || purpose.is_empty() || !digest.starts_with("sha256:") {
        return None;
    }
    Some((purpose, sources, digest))
}

/// `sha256:<hex>` over the sorted `<rel>\0<sha256>\n` record list.
pub fn sources_digest(root: &Path, sources: &[String]) -> Result<String> {
    let mut sorted: Vec<&str> = sources.iter().map(String::as_str).collect();
    sorted.sort_unstable();

    let mut records = String::new();
    for rel in sorted {
        let bytes = fs::read(root.join(rel)).with_context(|| format!("reading {rel}"))?;
        records.push_str(rel);
        records.push('\0');
        records.push_str(&hex(&Sha256::digest(&bytes)));
        records.push('\n');
    }
    Ok(format!(
        "sha256:{}",
        hex(&Sha256::digest(records.as_bytes()))
    ))
}

/// Lockfiles per package manager, and the marker that manager writes inside
/// `node_modules` once an install completed.
const JS_LOCKFILES: &[(&str, &[&str])] = &[
    ("npm", &["package-lock.json", "npm-shrinkwrap.json"]),
    ("pnpm", &["pnpm-lock.yaml"]),
    ("yarn", &["yarn.lock"]),
    ("bun", &["bun.lock", "bun.lockb"]),
];

const JS_MARKERS: &[(&str, &[&str])] = &[
    ("npm", &[".package-lock.json"]),
    ("pnpm", &[".modules.yaml"]),
    ("yarn", &[".yarn-integrity"]),
    ("bun", &[".bun-install"]),
];

/// Whether an install is warranted: `node_modules` is missing, or a lockfile is
/// newer than the package manager's own completion marker. A marker-less
/// `node_modules` is *unknown*, never stale — srvm does not reinstall on a
/// guess.
pub fn js_install_needed(root: &Path, manager: &str) -> bool {
    let modules = root.join("node_modules");
    if !modules.is_dir() {
        return true;
    }
    let Some(marker) = lookup(JS_MARKERS, manager).map(|name| modules.join(name)) else {
        return false;
    };
    let Ok(marker_time) = modified(&marker) else {
        return false;
    };
    let Some(lock_time) = newest_lockfile(root, manager) else {
        return false;
    };
    lock_time > marker_time
}

/// The newest timestamp among every lockfile name `manager` may use: npm has
/// both `package-lock.json` and `npm-shrinkwrap.json`, bun has both `bun.lock`
/// and `bun.lockb`. `None` means no lockfile is present at all, which is
/// unknown evidence and never triggers an install on its own.
fn newest_lockfile(root: &Path, manager: &str) -> Option<SystemTime> {
    let names = JS_LOCKFILES
        .iter()
        .find(|(name, _)| *name == manager)
        .map(|(_, names)| *names)?;
    names
        .iter()
        .filter_map(|name| modified(&root.join(name)).ok())
        .max()
}

fn lookup(
    table: &'static [(&'static str, &'static [&'static str])],
    manager: &str,
) -> Option<&'static str> {
    table
        .iter()
        .find(|(name, _)| *name == manager)
        .and_then(|(_, names)| names.first().copied())
}

fn modified(path: &Path) -> Result<SystemTime> {
    Ok(fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .modified()?)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn write_aged(root: &Path, rel: &str, body: &str, age: Duration) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        let when = SystemTime::now() - age;
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn sources(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn every_lockfile_name_counts_for_a_manager() {
        let dir = tempdir().unwrap();
        write_aged(
            dir.path(),
            "node_modules/.package-lock.json",
            "{}",
            Duration::from_secs(600),
        );
        write_aged(
            dir.path(),
            "node_modules/.bun-install",
            "{}",
            Duration::from_secs(600),
        );
        assert!(
            !js_install_needed(dir.path(), "npm"),
            "no lockfile is unknown evidence, not a reason to reinstall"
        );
        // npm's second lockfile name counts too.
        write_aged(
            dir.path(),
            "npm-shrinkwrap.json",
            "{}",
            Duration::from_secs(60),
        );
        assert!(js_install_needed(dir.path(), "npm"));
        assert!(!js_install_needed(dir.path(), "bun"));
        write_aged(dir.path(), "bun.lockb", "{}", Duration::from_secs(60));
        assert!(js_install_needed(dir.path(), "bun"));
    }

    #[test]
    fn an_interrupted_install_is_repaired_not_trusted() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "django\n").unwrap();
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));
        fs::create_dir_all(dir.path().join(".venv")).unwrap();

        record_incomplete(dir.path(), &stamp).unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Incomplete);

        record(dir.path(), &stamp).unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Fresh);
    }

    #[test]
    fn stamp_round_trips_through_its_wire_format() {
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));
        let line = format_stamp(&stamp, "sha256:abc");

        assert_eq!(
            line,
            "srvm-bootstrap-v1\tpython-requirements\trequirements.txt\tsha256:abc\n"
        );
        assert_eq!(
            parse(&line),
            Some((
                "python-requirements".to_string(),
                sources(&["requirements.txt"]),
                "sha256:abc".to_string()
            ))
        );
    }

    #[test]
    fn unparsable_stamps_are_rejected() {
        for text in [
            "",
            "srvm-bootstrap-v2\tpython-requirements\trequirements.txt\tsha256:abc\n",
            "srvm-bootstrap-v1\tpython-requirements\trequirements.txt\n",
            "srvm-bootstrap-v1\t\trequirements.txt\tsha256:abc\n",
            "srvm-bootstrap-v1\tpython-requirements\trequirements.txt\tabc\n",
            "srvm-bootstrap-v1\tpython-requirements\trequirements.txt\tsha256:abc\textra\n",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn digest_tracks_content_and_ignores_source_order() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();

        let forward = sources_digest(dir.path(), &sources(&["a.txt", "b.txt"])).unwrap();
        let reversed = sources_digest(dir.path(), &sources(&["b.txt", "a.txt"])).unwrap();
        assert_eq!(forward, reversed);

        fs::write(dir.path().join("b.txt"), "changed").unwrap();
        assert_ne!(
            forward,
            sources_digest(dir.path(), &sources(&["a.txt", "b.txt"])).unwrap()
        );
    }

    #[test]
    fn a_missing_stamp_is_unknown_not_stale() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));

        assert_eq!(state(dir.path(), &stamp), StampState::Missing);
    }

    #[test]
    fn a_recorded_stamp_is_fresh_until_its_sources_change() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));
        fs::create_dir_all(dir.path().join(".venv")).unwrap();

        record(dir.path(), &stamp).unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Fresh);

        fs::write(dir.path().join("requirements.txt"), "flask\ndjango\n").unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Stale);
    }

    #[test]
    fn a_garbled_or_foreign_stamp_is_unknown() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));
        fs::create_dir_all(dir.path().join(".venv")).unwrap();
        let path = dir.path().join(".venv").join(STAMP_FILE);

        fs::write(&path, "not a stamp\n").unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Unknown);

        fs::write(
            &path,
            "srvm-bootstrap-v1\tpython-poetry\tpyproject.toml\tsha256:abc\n",
        )
        .unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Unknown);
    }

    #[test]
    fn a_stamp_whose_sources_vanished_is_unknown() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        let stamp = Stamp::python_venv(Path::new(".venv"), &sources(&["requirements.txt"]));
        fs::create_dir_all(dir.path().join(".venv")).unwrap();
        record(dir.path(), &stamp).unwrap();

        fs::remove_file(dir.path().join("requirements.txt")).unwrap();
        assert_eq!(state(dir.path(), &stamp), StampState::Unknown);
    }

    #[test]
    fn javascript_installs_only_when_modules_are_missing_or_stale() {
        let dir = tempdir().unwrap();
        assert!(js_install_needed(dir.path(), "npm"));

        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        assert!(
            !js_install_needed(dir.path(), "npm"),
            "a marker-less node_modules is unknown, never stale"
        );

        write_aged(
            dir.path(),
            "node_modules/.package-lock.json",
            "{}",
            Duration::from_secs(600),
        );
        assert!(
            !js_install_needed(dir.path(), "npm"),
            "older lockfile, nothing to do"
        );

        write_aged(
            dir.path(),
            "package-lock.json",
            "{}",
            Duration::from_secs(300),
        );
        assert!(
            js_install_needed(dir.path(), "npm"),
            "a lockfile newer than the marker means stale modules"
        );
    }

    #[test]
    fn an_unknown_package_manager_never_triggers_an_install() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        assert!(!js_install_needed(dir.path(), "deno"));
        assert!(!js_install_needed(dir.path(), "bun"));
    }
}
