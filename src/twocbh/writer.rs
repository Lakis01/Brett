//! Native writer for ChessBase's current multi-file database format
//! (`.2cbh`, `.2cbg`, `.2cba`, and `.2lid`).
//!
//! Games are streamed to temporary files beside the target.  On completion the
//! existing companion files at the target stem are moved aside to backup paths
//! and replaced atomically, rolling back on error.  Derived `.2lcd`/`.2lgd`
//! indexes are removed with the backups so ChessBase rebuilds them.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::domain::Game;

use super::{CBG_MAGIC, LID_FIRST, LID_STRIDE, RECORD_SIZE, sibling};

/// The four files that make up a 2CBH database.
pub const CORE_EXTENSIONS: [&str; 4] = ["2cbh", "2cbg", "2cba", "2lid"];

/// ChessBase index files derived from a 2CBH database.
pub const DERIVED_EXTENSIONS: [&str; 2] = ["2lcd", "2lgd"];

/// Every file that belongs to a 2CBH stem (core files plus derived indexes).
pub const ALL_EXTENSIONS: [&str; 6] = ["2cbh", "2cbg", "2cba", "2lid", "2lcd", "2lgd"];

const CBG_HEADER: u64 = 12;
const CBG_VERSION: u16 = 0x0500;
const CBA_VERSION: u16 = 0;
const RECORD_ZERO_GENERATION: u8 = 0x26;

/// Summary of a completed 2CBH export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TwoCbhWriteReport {
    pub games: usize,
    pub bytes: u64,
}

/// Options controlling what a 2CBH export may discard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TwoCbhWriteOptions {
    /// Allow writing games whose comments, NAGs, clocks, or evaluations are
    /// not representable in this container-first pass.
    pub allow_annotation_loss: bool,
}

/// Return the four core companion paths for a `.2cbh` path or bare stem.
pub fn core_companion_files(path: &Path) -> [PathBuf; 4] {
    let stem = lenient_stem(path);
    CORE_EXTENSIONS.map(|ext| sibling(&stem, &format!(".{ext}")))
}

/// Return every existing 2CBH companion file sharing `path`'s stem.
pub fn existing_companion_files(path: &Path) -> Vec<PathBuf> {
    let stem = lenient_stem(path);
    let mut out = Vec::new();
    for ext in ALL_EXTENSIONS {
        let candidate = sibling(&stem, &format!(".{ext}"));
        if candidate.exists() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// Write a 2CBH database at `path`, refusing annotation loss.
pub fn write_database<I>(path: &Path, games: I) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    write_database_with_options(path, TwoCbhWriteOptions::default(), games)
}

/// Write a 2CBH database at `path` with explicit options.
pub fn write_database_with_options<I>(
    path: &Path,
    options: TwoCbhWriteOptions,
    games: I,
) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    let stem = normalize_stem(path)?;
    if let Some(parent) = stem.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        bail!("2CBH export directory {} does not exist", parent.display());
    }

    let writer = TwoCbhWriter::create(&stem, options)?;
    if let Some(game) = games.into_iter().next() {
        let game = game?;
        bail!(
            "2CBH game writing is not implemented yet (refusing to drop {} - {})",
            game.white(),
            game.black()
        );
    }
    writer.finish()
}

fn normalize_stem(path: &Path) -> Result<PathBuf> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if ext.eq_ignore_ascii_case("2cbh")
                || ext.eq_ignore_ascii_case("2cbg")
                || ext.eq_ignore_ascii_case("2cba")
                || ext.eq_ignore_ascii_case("2lid") =>
        {
            Ok(path.with_extension(""))
        }
        Some(_) => bail!(
            "2CBH export target must end in .2cbh or name a database stem (got {})",
            path.display()
        ),
        None => Ok(path.to_path_buf()),
    }
}

fn lenient_stem(path: &Path) -> PathBuf {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if CORE_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate)) =>
        {
            path.with_extension("")
        }
        _ => path.to_path_buf(),
    }
}

/// Append an extension without replacing a dotted stem's last component.
fn core_path(stem: &Path, ext: &str) -> PathBuf {
    let mut path = stem.as_os_str().to_owned();
    path.push(ext);
    PathBuf::from(path)
}

/// Frame one `.2cbg` or `.2cba` record at `offset` with the 2CBH padding rule.
#[allow(dead_code)] // Task 4 calls this from `add_game`.
fn frame_record(
    content: &[u8],
    terminator: [u8; 2],
    base_padding: u64,
    key: u64,
    offset: u64,
) -> Vec<u8> {
    let a = content.len() as u64;
    let b = base_padding + (8 - ((offset + 34 + a + base_padding) % 8)) % 8;
    let total = 34 + a + b;
    debug_assert!(a <= u64::from(u32::MAX), "2CBH record content is too large");
    debug_assert!(b <= u64::from(u32::MAX), "2CBH record padding is too large");

    // magic (8), A (4), B (4), key (8), content (a), terminator (2), zero
    // padding (b), trailer (8).
    let mut record = Vec::with_capacity(total as usize);
    record.extend_from_slice(&CBG_MAGIC);
    record.extend_from_slice(&(a as u32).to_le_bytes());
    record.extend_from_slice(&(b as u32).to_le_bytes());
    record.extend_from_slice(&key.to_be_bytes());
    record.extend_from_slice(content);
    record.extend_from_slice(&terminator);
    record.resize(record.len() + b as usize, 0);
    record.extend_from_slice(&total.to_le_bytes());
    debug_assert_eq!(record.len() as u64, total);
    record
}

/// Deterministic non-zero opaque key for one `.2cbg` record.
#[allow(dead_code)] // Task 4 uses this for `.2cbg` record keys.
fn record_key(index: u64, content: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in index.to_le_bytes().iter().chain(content) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if hash == 0 { 1 } else { hash }
}

struct TwoCbhWriter {
    stem: PathBuf,
    unique: String,
    cbh_tmp: PathBuf,
    cbg_tmp: PathBuf,
    cba_tmp: PathBuf,
    lid_tmp: PathBuf,
    cbh_file: Option<BufWriter<File>>,
    cbg_file: Option<BufWriter<File>>,
    cba_file: Option<BufWriter<File>>,
    lid_file: Option<File>,
    cbg_offset: u64,
    cba_offset: u64,
    games: u32,
    block_count: u32,
    event_count: u32,
    guard: TempGuard,
}

impl TwoCbhWriter {
    fn create(stem: &Path, _options: TwoCbhWriteOptions) -> Result<Self> {
        // The annotation-loss guard added in Task 5 reads `_options`; accepting
        // it now keeps the public API stable across the skeleton and the guard.
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let cbh_tmp = temp_path(&core_path(stem, ".2cbh"), &unique);
        let cbg_tmp = temp_path(&core_path(stem, ".2cbg"), &unique);
        let cba_tmp = temp_path(&core_path(stem, ".2cba"), &unique);
        let lid_tmp = temp_path(&core_path(stem, ".2lid"), &unique);

        let guard = TempGuard {
            paths: vec![
                cbh_tmp.clone(),
                cbg_tmp.clone(),
                cba_tmp.clone(),
                lid_tmp.clone(),
            ],
            committed: false,
        };

        let mut cbh_file = BufWriter::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&cbh_tmp)
                .with_context(|| format!("creating temporary 2CBH header {}", cbh_tmp.display()))?,
        );
        // Reserve record 0; the final record count is patched at finish.
        cbh_file.write_all(&cbh_record_zero(0))?;

        let mut cbg_file = BufWriter::new(
            File::create(&cbg_tmp)
                .with_context(|| format!("creating temporary 2CBG moves {}", cbg_tmp.display()))?,
        );
        cbg_file.write_all(&companion_header(0, CBG_VERSION))?;

        let mut cba_file = BufWriter::new(File::create(&cba_tmp).with_context(|| {
            format!("creating temporary 2CBA annotations {}", cba_tmp.display())
        })?);
        cba_file.write_all(&companion_header(0, CBA_VERSION))?;

        let mut lid_file = File::create(&lid_tmp)
            .with_context(|| format!("creating temporary 2LID names {}", lid_tmp.display()))?;
        lid_file.write_all(&lid_header(1, 0))?;

        Ok(Self {
            stem: stem.to_path_buf(),
            unique,
            cbh_tmp,
            cbg_tmp,
            cba_tmp,
            lid_tmp,
            cbh_file: Some(cbh_file),
            cbg_file: Some(cbg_file),
            cba_file: Some(cba_file),
            lid_file: Some(lid_file),
            cbg_offset: CBG_HEADER,
            cba_offset: CBG_HEADER,
            games: 0,
            block_count: 1,
            event_count: 0,
            guard,
        })
    }

    fn finish(mut self) -> Result<TwoCbhWriteReport> {
        let mut cbh_writer = self
            .cbh_file
            .take()
            .context("2CBH header temporary file is already closed")?;
        cbh_writer.flush()?;
        let mut cbh_file = cbh_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBH header: {err}"))?;
        cbh_file.seek(SeekFrom::Start(0))?;
        cbh_file.write_all(&cbh_record_zero(
            self.games
                .checked_add(1)
                .context("2CBH record count overflow")?,
        ))?;
        cbh_file.flush()?;
        drop(cbh_file);

        let mut cbg_writer = self
            .cbg_file
            .take()
            .context("2CBG temporary file is already closed")?;
        cbg_writer.flush()?;
        let mut cbg_file = cbg_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBG moves: {err}"))?;
        cbg_file.seek(SeekFrom::Start(0))?;
        cbg_file.write_all(&companion_header(self.cbg_offset, CBG_VERSION))?;
        cbg_file.flush()?;
        drop(cbg_file);

        let mut cba_writer = self
            .cba_file
            .take()
            .context("2CBA temporary file is already closed")?;
        cba_writer.flush()?;
        let mut cba_file = cba_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBA annotations: {err}"))?;
        cba_file.seek(SeekFrom::Start(0))?;
        cba_file.write_all(&companion_header(self.cba_offset, CBA_VERSION))?;
        cba_file.flush()?;
        drop(cba_file);

        let mut lid_file = self
            .lid_file
            .take()
            .context("2LID temporary file is already closed")?;
        let lid_len = LID_FIRST as u64 + u64::from(self.block_count) * LID_STRIDE as u64;
        lid_file.set_len(lid_len)?;
        lid_file.seek(SeekFrom::Start(0))?;
        lid_file.write_all(&lid_header(self.block_count, self.event_count))?;
        lid_file.flush()?;
        drop(lid_file);

        // Install `.2cbh` last so readers never observe a new header before
        // its companion files are in place.
        let staged = [
            (&self.cbg_tmp, core_path(&self.stem, ".2cbg")),
            (&self.cba_tmp, core_path(&self.stem, ".2cba")),
            (&self.lid_tmp, core_path(&self.stem, ".2lid")),
            (&self.cbh_tmp, core_path(&self.stem, ".2cbh")),
        ];

        let mut total_bytes = 0u64;
        for (tmp, _) in &staged {
            total_bytes += fs::metadata(tmp).map(|meta| meta.len()).unwrap_or(0);
        }

        replace_companion_set(&self.stem, &staged, &self.unique)?;
        self.guard.committed = true;

        Ok(TwoCbhWriteReport {
            games: self.games as usize,
            bytes: total_bytes,
        })
    }
}

fn cbh_record_zero(count: u32) -> [u8; RECORD_SIZE] {
    let mut header = [0u8; RECORD_SIZE];
    header[0x08] = RECORD_ZERO_GENERATION;
    header[0x0a..0x0c].copy_from_slice(&(RECORD_SIZE as u16).to_le_bytes());
    header[0x0c..0x0e].copy_from_slice(&CBG_VERSION.to_le_bytes());
    header[0x10..0x14].copy_from_slice(&count.to_le_bytes());
    header
}

fn companion_header(length: u64, version: u16) -> [u8; CBG_HEADER as usize] {
    let mut header = [0u8; CBG_HEADER as usize];
    header[0..8].copy_from_slice(&length.to_le_bytes());
    header[8..10].copy_from_slice(&(CBG_HEADER as u16).to_le_bytes());
    header[10..12].copy_from_slice(&version.to_le_bytes());
    header
}

fn lid_header(block_count: u32, event_count: u32) -> [u8; LID_FIRST] {
    let mut header = [0u8; LID_FIRST];
    put_be(&mut header, 0x00, LID_FIRST as u32);
    put_be(&mut header, 0x04, 6);
    put_be(&mut header, 0x08, 0x400);
    put_be(&mut header, 0x0c, 0);
    put_be(&mut header, 0x10, block_count.saturating_add(1));
    put_be(&mut header, 0x14, u32::MAX);
    put_be(&mut header, 0x18, u32::MAX);
    put_be(&mut header, 0x1c, 0x460);
    put_be(&mut header, 0x24, event_count);
    put_be(&mut header, 0x38, 2);
    put_be(&mut header, 0x44, 0x400);
    put_be(&mut header, 0x74, 1);
    put_be(&mut header, 0x80, u32::MAX);
    header
}

fn put_be(header: &mut [u8], at: usize, value: u32) {
    header[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn replace_companion_set(stem: &Path, staged: &[(&PathBuf, PathBuf)], unique: &str) -> Result<()> {
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ext in ALL_EXTENSIONS {
        let existing = sibling(stem, &format!(".{ext}"));
        if existing.exists() {
            let backup = temp_path(&existing, &format!("bak-{unique}"));
            if let Err(err) = fs::rename(&existing, &backup) {
                restore_backups(&backups);
                return Err(err).with_context(|| {
                    format!(
                        "moving existing 2CBH file {} aside before replacement",
                        existing.display()
                    )
                });
            }
            backups.push((existing, backup));
        }
    }

    let mut installed: Vec<PathBuf> = Vec::new();
    for (tmp, target) in staged {
        if let Err(err) = fs::rename(tmp, target) {
            for placed in &installed {
                let _ = fs::remove_file(placed);
            }
            restore_backups(&backups);
            return Err(err).with_context(|| {
                format!(
                    "installing new 2CBH file {} from {}",
                    target.display(),
                    tmp.display()
                )
            });
        }
        installed.push(target.clone());
    }

    for (_, backup) in backups {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn restore_backups(backups: &[(PathBuf, PathBuf)]) {
    for (original, backup) in backups.iter().rev() {
        let _ = fs::rename(backup, original);
    }
}

struct TempGuard {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn temp_path(target: &Path, unique: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("2cbh-export");
    target.with_file_name(format!(".{name}.{unique}.tmp"))
}

#[cfg(test)]
mod tests;
