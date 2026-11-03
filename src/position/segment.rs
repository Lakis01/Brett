//! Immutable sorted position index segment (`.bpx` or `.brettpos`).
//!
//! A segment is an immutable memory-mapped file consisting of an 80-byte
//! [`IndexHeader`] followed by `edge_count` sorted 120-byte [`EdgeRecord`]s.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memmap2::{Mmap, MmapOptions};

use super::{EDGE_SIZE, EdgeRecord, HEADER_LEN, IndexHeader, read_edge, write_edge};

pub struct IndexSegment {
    mmap: Mmap,
    header: IndexHeader,
    path: PathBuf,
}

impl IndexSegment {
    /// Open a segment file, validating its magic, version, and length.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("opening segment {}", path.display()))?;
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("mapping segment {}", path.display()))?;
        let header = IndexHeader::parse(&mmap)
            .with_context(|| format!("reading segment header from {}", path.display()))?;
        let expected = HEADER_LEN
            .checked_add(
                usize::try_from(header.edge_count)
                    .context("segment edge count does not fit in memory")?
                    .checked_mul(EDGE_SIZE)
                    .context("segment length overflow")?,
            )
            .context("segment length overflow")?;
        if mmap.len() < expected {
            bail!(
                "segment {} is truncated: {} bytes, expected at least {}",
                path.display(),
                mmap.len(),
                expected
            );
        }
        Ok(Self {
            mmap,
            header,
            path: path.to_path_buf(),
        })
    }

    /// Write an array of sorted records and their header to disk.
    pub(crate) fn write_records(
        path: impl AsRef<Path>,
        header: IndexHeader,
        records: &[EdgeRecord],
    ) -> Result<()> {
        let path = path.as_ref();
        let file = File::create(path).with_context(|| format!("creating segment {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&header.to_bytes())?;
        let mut buf = [0u8; EDGE_SIZE];
        for record in records {
            write_edge(&mut buf, record);
            writer.write_all(&buf)?;
        }
        writer.flush()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn header(&self) -> &IndexHeader {
        &self.header
    }

    pub fn edge_count(&self) -> usize {
        self.header.edge_count as usize
    }

    pub fn game_count(&self) -> usize {
        self.header.game_count as usize
    }

    pub fn position_count(&self) -> usize {
        self.header.position_count as usize
    }

    pub fn file_len(&self) -> u64 {
        self.mmap.len() as u64
    }

    pub(crate) fn edge_at(&self, index: usize) -> EdgeRecord {
        let start = HEADER_LEN + index * EDGE_SIZE;
        read_edge(&self.mmap[start..start + EDGE_SIZE])
    }

    pub fn lower_bound(&self, key: u64) -> usize {
        let mut lo = 0;
        let mut hi = self.edge_count();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.edge_at(mid).key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub fn upper_bound(&self, key: u64) -> usize {
        let mut lo = 0;
        let mut hi = self.edge_count();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.edge_at(mid).key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub(crate) fn matching_records(&self, key: u64) -> Vec<EdgeRecord> {
        let start = self.lower_bound(key);
        let end = self.upper_bound(key);
        (start..end).map(|i| self.edge_at(i)).collect()
    }
}
