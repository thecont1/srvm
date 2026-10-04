use std::{
    fs,
    io::{self},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

pub fn extract_verified(bytes: &[u8], expected: &[u8; 32], dest: &Path) -> Result<()> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    if &digest != expected {
        bail!("checksum mismatch; refusing to unpack");
    }
    fs::create_dir_all(dest)?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz(bytes, dest)
    } else if bytes.starts_with(b"PK") {
        extract_zip(bytes, dest)
    } else {
        bail!("archive is neither gzip nor zip")
    }
}

pub fn find_tool(root: &Path, names: &[&str]) -> Option<PathBuf> {
    let wanted = names
        .iter()
        .flat_map(|name| tool_names(name))
        .collect::<Vec<_>>();
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
            let file_name = path.file_name()?.to_string_lossy();
            if wanted.iter().any(|name| file_name == *name) {
                return Some(path);
            }
        }
    }
    None
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
            header.set_mode(0o644);
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
