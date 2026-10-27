//! ChessBase `.cbv` database archives.
//!
//! A `.cbv` is a container around a classic `.cbh` database.  Brett does not
//! read games out of the container directly: it materialises the handful of
//! database members that the classic reader needs into a private cache and then
//! opens the extracted `.cbh` through [`crate::cbh::CbhLibrary`].  Assets such
//! as board bitmaps and HTML reports are deliberately not extracted.
//!
//! The container codec is the clean-room, MIT-licensed `cbvault-format` crate.
//! Keeping the extraction here means a first open is the only expensive step;
//! later opens reuse the cache and are as fast as a normal classic database.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use cbvault_format::archive::Archive;

/// Database members the classic reader can consume.  `.cbj` is only needed
/// when one of the large streams crosses the 4 GiB boundary.
const DATABASE_EXTENSIONS: &[&str] = &["cbh", "cbg", "cba", "cbp", "cbt", "cbc", "cbs"];

/// Materialise the database members of `archive` and return the path of the
/// extracted `.cbh`.
///
/// The cache lives under `$XDG_CACHE_HOME/brett/cbv` (or the platform cache
/// fallback).  It is keyed by the archive's canonical path, size and mtime, so
/// replacing the archive never silently reuses stale data.
pub fn materialize(archive: &Path) -> Result<PathBuf> {
    materialize_with_progress(archive, |_| {})
}

/// Same as [`materialize`], with a callback for the member currently being
/// decoded.  The callback runs on the caller's thread and is intended for a
/// status line or progress channel.
pub fn materialize_with_progress(
    archive: &Path,
    mut progress: impl FnMut(&str),
) -> Result<PathBuf> {
    let metadata = fs::metadata(archive)
        .with_context(|| format!("reading ChessBase archive metadata {}", archive.display()))?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let since_epoch = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let cache_dir = cache_root().join(cache_key(archive, metadata.len(), since_epoch.as_secs()));
    let complete = cache_dir.join(".complete");
    let cached_cbh = cache_dir.join("database.cbh");

    if complete.is_file() && cached_cbh.is_file() {
        return Ok(cached_cbh);
    }
    if cache_dir.exists() {
        // A previous extraction was interrupted.  Never mix its files with a
        // fresh attempt.
        let _ = fs::remove_dir_all(&cache_dir);
    }

    let archive_data = Archive::open(archive)
        .with_context(|| format!("opening ChessBase archive {}", archive.display()))?;
    let root: Vec<_> = archive_data
        .list()
        .iter()
        .filter(|member| !member.is_asset())
        .collect();
    let wide_needed = root.iter().any(|member| {
        let ext = extension(member.name());
        (ext == "cbg" || ext == "cba") && member.size() >= (1u64 << 32)
    });
    let needed: Vec<_> = root
        .into_iter()
        .filter(|member| {
            let ext = extension(member.name());
            DATABASE_EXTENSIONS.contains(&ext.as_str()) || (wide_needed && ext == "cbj")
        })
        .collect();
    if !needed
        .iter()
        .any(|member| extension(member.name()) == "cbh")
    {
        bail!(
            "{} does not contain a classic ChessBase .cbh database",
            archive.display()
        );
    }

    let parent = cache_dir
        .parent()
        .context("the ChessBase cache directory has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("creating ChessBase cache {}", parent.display()))?;
    let staging = cache_dir.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)
        .with_context(|| format!("creating ChessBase staging directory {}", staging.display()))?;

    let extract_result = (|| -> Result<()> {
        for member in &needed {
            progress(member.name());
            let bytes = archive_data
                .decode(member)
                .with_context(|| format!("decoding archive member {}", member.name()))?;
            // The classic reader discovers companions from the `.cbh` stem.
            // Rename every extracted member to that same stem so the archive's
            // original base name (which can contain spaces) is irrelevant.
            let name = format!("database.{}", extension(member.name()));
            let destination = staging.join(&name);
            let file = File::create(&destination)
                .with_context(|| format!("creating {}", destination.display()))?;
            let mut writer = BufWriter::new(file);
            writer
                .write_all(&bytes)
                .with_context(|| format!("writing {}", destination.display()))?;
            writer
                .flush()
                .with_context(|| format!("flushing {}", destination.display()))?;
        }
        let marker = staging.join(".complete");
        fs::write(&marker, archive.display().to_string())
            .with_context(|| format!("writing cache marker {}", marker.display()))?;
        Ok(())
    })();

    if let Err(err) = extract_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }

    fs::rename(&staging, &cache_dir).with_context(|| {
        format!(
            "installing ChessBase cache {} -> {}",
            staging.display(),
            cache_dir.display()
        )
    })?;
    Ok(cached_cbh)
}

/// The extracted cache directory, for diagnostics and cleanup commands.
pub fn cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("brett").join("cbv");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("brett").join("cbv");
    }
    std::env::temp_dir().join("brett-cbv")
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn cache_key(path: &Path, len: u64, modified_secs: u64) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in path.as_os_str().to_string_lossy().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for value in [len, modified_secs] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_metadata() {
        let path = Path::new("/tmp/example.cbv");
        assert_ne!(
            cache_key(path, 1, 2),
            cache_key(path, 1, 3),
            "mtime must invalidate the cache"
        );
        assert_ne!(
            cache_key(path, 1, 2),
            cache_key(path, 2, 2),
            "size must invalidate the cache"
        );
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert_eq!(extension("Mega Database 2025.CBH"), "cbh");
        assert_eq!(extension("no-extension"), "");
    }
}
