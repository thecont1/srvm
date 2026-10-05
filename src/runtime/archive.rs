use std::{
    fs,
    io::{self},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

pub fn extract_verified(bytes: &[u8], expected: &[u8; 32], dest: &Path) -> Result<()> {
    verify_digest(bytes, expected)?;
    fs::create_dir_all(dest)?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz(bytes, dest)
    } else if bytes.starts_with(b"PK") {
        extract_zip(bytes, dest)
    } else {
        bail!("archive is neither gzip nor zip")
    }
}

/// Like `extract_verified`, but merges each component's top-level directory
/// into `dest` itself. Rust toolchains ship rustc, cargo and rust-std as
/// separate archives whose contents must share one prefix (bin/, lib/…), so
/// that rustc finds the standard library relative to its own binary.
pub fn extract_verified_flat(bytes: &[u8], expected: &[u8; 32], dest: &Path) -> Result<()> {
    extract_verified(bytes, expected, dest)?;
    let top_dirs = fs::read_dir(dest)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    for dir in top_dirs {
        merge_up(&dir, dest)?;
    }
    Ok(())
}

fn verify_digest(bytes: &[u8], expected: &[u8; 32]) -> Result<()> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    if &digest != expected {
        bail!("checksum mismatch; refusing to unpack");
    }
    Ok(())
}

/// Move everything under `src` into `dest`, recursing into directories that
/// already exist there, then remove `src`. Used to flatten component
/// archives into a shared toolchain prefix.
fn merge_up(src: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if target.is_dir() {
                merge_up(&entry.path(), &target)?;
            } else {
                fs::rename(entry.path(), &target)?;
            }
        } else {
            if target.exists() {
                fs::remove_file(&target)?;
            }
            fs::rename(entry.path(), &target)?;
        }
    }
    fs::remove_dir(src)?;
    Ok(())
}

pub fn find_tool(root: &Path, names: &[&str]) -> Option<PathBuf> {
    let wanted = names
        .iter()
        .flat_map(|name| tool_names(name))
        .collect::<Vec<_>>();
    // Prefer the canonical toolchain locations — a `bin` directory or the
    // search root itself — before accepting any filename match deeper in the
    // tree, so a stray shim or same-named file cannot shadow the real binary.
    find_tool_in(root, &wanted, true).or_else(|| find_tool_in(root, &wanted, false))
}

fn find_tool_in(root: &Path, wanted: &[String], preferred_only: bool) -> Option<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0u8)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < 6 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            // Windows tools are real files; reading through tar-created
            // symlinks can fail there, and CreateProcess would not follow
            // them to launchable content anyway. Unix keeps them — node's
            // bin/npm is a symlink and is the canonical match.
            #[cfg(windows)]
            if file_type.is_symlink() {
                continue;
            }
            let file_name = path.file_name()?.to_string_lossy();
            if wanted.iter().any(|name| file_name == *name)
                && (!preferred_only || is_preferred_location(&path, root))
                && has_executable_bit(&path)
            {
                return Some(path);
            }
        }
    }
    None
}

/// Canonical tool locations: files directly inside a `bin` directory or
/// directly at the search root.
fn is_preferred_location(path: &Path, root: &Path) -> bool {
    match path.parent() {
        Some(parent) if parent == root => true,
        Some(parent) => parent
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("bin")),
        None => false,
    }
}

/// On Unix a file that cannot be executed cannot serve as the tool; require
/// at least one execute bit (following symlinks, matching extraction modes).
/// Windows has no such concept, so every file qualifies.
fn has_executable_bit(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

fn tool_names(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            name.to_string(),
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![name.to_string()]
    }
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !entry_path_ok(&path) {
            bail!("refusing archive entry {}", path.display());
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let link = entry
                .link_name()?
                .with_context(|| format!("link {} has no target", path.display()))?;
            if !link_stays_inside(dest, &path, &link) {
                bail!("refusing symlink {} -> {}", path.display(), link.display());
            }
        }
        let unpacked = entry
            .unpack_in(dest)
            .with_context(|| format!("unpack {}", path.display()))?;
        if !unpacked {
            bail!("archive entry {} escaped the destination", path.display());
        }
    }
    Ok(())
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes))?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(rel) = file.enclosed_name() else {
            bail!("refusing zip entry that escapes the destination");
        };
        let out = dest.join(rel);
        if file.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut dest_file = fs::File::create(&out)?;
        io::copy(&mut file, &mut dest_file)?;
    }
    Ok(())
}

fn entry_path_ok(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir))
}

fn link_stays_inside(dest: &Path, entry: &Path, link: &Path) -> bool {
    if link.is_absolute() || link.components().any(|c| matches!(c, Component::RootDir)) {
        return false;
    }
    let joined = dest.join(entry);
    let parent = joined.parent().unwrap_or(dest);
    lexical_normalize(&parent.join(link)).starts_with(lexical_normalize(dest))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[test]
    fn rejects_checksum_mismatch_without_unpacking() {
        let dest = tempdir().unwrap();
        let bytes = b"not an archive";
        let expected = [0u8; 32];
        let err = extract_verified(bytes, &expected, dest.path()).unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(dest.path().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn extracts_tar_gz_and_finds_tool() {
        let body = b"#!/bin/sh\necho fixture-node\n";
        let archive = tar_gz(&[("node-v1.0.0-darwin-arm64/bin/node", body)]);
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        extract_verified(&archive, &digest, dest.path()).unwrap();
        let tool = find_tool(dest.path(), &["node"]).unwrap();
        assert_eq!(tool.file_name().unwrap(), "node");
        assert!(tool.is_file());
    }

    #[test]
    fn allows_relative_symlink_inside_the_archive() {
        let archive = tar_gz_with_symlink("node-v/lib/npm", b"cli", "node-v/bin/npm", "../lib/npm");
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        extract_verified(&archive, &digest, dest.path()).unwrap();
        let tool = find_tool(dest.path(), &["npm"]).unwrap();
        assert_eq!(fs::read(tool).unwrap(), b"cli");
    }

    #[test]
    fn rejects_symlink_that_escapes() {
        let archive = tar_gz_symlink("bin/evil", "/etc/passwd");
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        let err = extract_verified(&archive, &digest, dest.path()).unwrap_err();
        assert!(
            err.to_string().contains("refusing") || err.to_string().contains("escaped"),
            "{err}"
        );
        assert!(fs::symlink_metadata(dest.path().join("bin/evil")).is_err());
    }

    #[test]
    fn flat_extraction_merges_component_roots_into_one_prefix() {
        let archive = tar_gz(&[
            ("rustc/bin/rustc", b"compiler"),
            ("cargo/bin/cargo", b"builder"),
            ("rust-std-x/lib/rustlib/x/lib/libstd.rlib", b"stdlib"),
        ]);
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        extract_verified_flat(&archive, &digest, dest.path()).unwrap();

        assert_eq!(
            fs::read(dest.path().join("bin/rustc")).unwrap(),
            b"compiler"
        );
        assert_eq!(fs::read(dest.path().join("bin/cargo")).unwrap(), b"builder");
        assert_eq!(
            fs::read(dest.path().join("lib/rustlib/x/lib/libstd.rlib")).unwrap(),
            b"stdlib"
        );
        assert!(!dest.path().join("rustc").exists());
        assert!(!dest.path().join("cargo").exists());
        assert!(!dest.path().join("rust-std-x").exists());

        // The merged prefix is what find_tool searches: tools share bin/.
        let rustc = find_tool(dest.path(), &["rustc"]).unwrap();
        let cargo = find_tool(dest.path(), &["cargo"]).unwrap();
        assert_eq!(rustc.parent().unwrap(), cargo.parent().unwrap());
    }

    #[test]
    fn find_tool_prefers_bin_over_a_deeper_same_named_file() {
        let archive = tar_gz(&[
            ("node-v/bin/node", b"real"),
            ("node-v/lib/node", b"impostor"),
        ]);
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        extract_verified(&archive, &digest, dest.path()).unwrap();
        let tool = find_tool(dest.path(), &["node"]).unwrap();
        assert_eq!(fs::read(&tool).unwrap(), b"real");
    }

    #[cfg(unix)]
    #[test]
    fn find_tool_rejects_matches_without_the_execute_bit() {
        let archive = tar_gz(&[
            ("node-v/lib/npm", b"data file"),
            ("node-v/bin/node", b"#!/bin/sh\necho ok\n"),
        ]);
        let digest = sha256(&archive);
        let dest = tempdir().unwrap();
        extract_verified(&archive, &digest, dest.path()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dest.path().join("node-v/lib/npm"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        // "node" is executable in bin/ and wins; a bare non-executable match
        // elsewhere must not be returned instead of it.
        assert!(find_tool(dest.path(), &["node"]).is_some());

        // With only the non-executable file present, nothing qualifies.
        fs::remove_file(dest.path().join("node-v/bin/node")).unwrap();
        assert!(find_tool(dest.path(), &["node"]).is_none());
    }

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut gz = Vec::new();
        let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap();
        gz
    }

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            for (path, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append(&header, *body).unwrap();
            }
            builder.finish().unwrap();
        }
        gzip(&raw)
    }

    fn tar_gz_symlink(path: &str, target: &str) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_path(path).unwrap();
            header.set_link_name(target).unwrap();
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty()).unwrap();
            builder.finish().unwrap();
        }
        gzip(&raw)
    }

    fn tar_gz_with_symlink(file: &str, body: &[u8], link: &str, target: &str) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            let mut header = tar::Header::new_gnu();
            header.set_path(file).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, body).unwrap();

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_path(link).unwrap();
            link_header.set_link_name(target).unwrap();
            link_header.set_size(0);
            link_header.set_cksum();
            builder.append(&link_header, io::empty()).unwrap();
            builder.finish().unwrap();
        }
        gzip(&raw)
    }
}
