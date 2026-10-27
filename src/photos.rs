//! Player-photo pack (`.bin`) reader.
//!
//! Brett's pack is generated from the Scid photo archive `Players.spf` by
//! `scripts/convert_spf.py`. The format is deliberately simple:
//!
//! ```text
//! "BPF1" | u32 entry count | entries | WebP data
//! entry: u8 key length | key bytes | u32 offset | u32 length
//! ```
//!
//! Keys use the same canonical player identity as [`crate::players::player_key`]
//! (lowercase words, sorted, punctuation removed), and entries are sorted by
//! key for binary search. Offsets are absolute file positions.
//!
//! The photo data is **not** part of Brett's MIT-licensed source. See
//! `docs/PHOTOS.md` and `THIRD_PARTY_NOTICES.md`.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::players::player_key;

const MAGIC: &[u8; 4] = b"BPF1";
const HEADER_LEN: usize = 8;

#[derive(Clone, Debug)]
struct PhotoEntry {
    key: String,
    offset: usize,
    len: usize,
}

/// A read-only, memory-mapped player-photo pack.
pub struct PhotoLibrary {
    mmap: Mmap,
    entries: Vec<PhotoEntry>,
    path: PathBuf,
}

impl PhotoLibrary {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)
            .with_context(|| format!("opening the player photo pack {}", path.display()))?;
        // The pack is read-only for the lifetime of the mapping. The file is
        // never modified by Brett.
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("mapping the player photo pack {}", path.display()))?;

        if mmap.len() < HEADER_LEN {
            bail!("{} is too short to be a photo pack", path.display());
        }
        if &mmap[..4] != MAGIC {
            bail!("{} is not a Brett photo pack (bad magic)", path.display());
        }
        let count = u32::from_le_bytes(
            mmap[4..8]
                .try_into()
                .expect("the header length was checked"),
        ) as usize;

        let mut cursor = HEADER_LEN;
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let key_len = *mmap
                .get(cursor)
                .with_context(|| format!("photo entry {index} is truncated"))?
                as usize;
            cursor += 1;
            let key_bytes = mmap
                .get(cursor..cursor + key_len)
                .with_context(|| format!("photo entry {index} has a truncated key"))?;
            let key = std::str::from_utf8(key_bytes)
                .with_context(|| format!("photo entry {index} has a non-UTF-8 key"))?
                .to_string();
            cursor += key_len;

            let offset = read_u32(&mmap, &mut cursor, index)? as usize;
            let len = read_u32(&mmap, &mut cursor, index)? as usize;
            let end = offset
                .checked_add(len)
                .with_context(|| format!("photo entry {index} has an overflowing range"))?;
            if end > mmap.len() {
                bail!(
                    "photo entry {index} points past the end of {} ({}..{})",
                    path.display(),
                    offset,
                    end
                );
            }
            entries.push(PhotoEntry { key, offset, len });
        }

        if !entries.windows(2).all(|pair| pair[0].key <= pair[1].key) {
            entries.sort_by(|left, right| left.key.cmp(&right.key));
        }

        Ok(Self {
            mmap,
            entries,
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a player's encoded WebP bytes by display name.
    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        let key = player_key(name)?;
        self.bytes_for_key(&key)
    }

    /// Find a player's encoded WebP bytes by canonical key.
    pub fn bytes_for_key(&self, key: &str) -> Option<&[u8]> {
        let index = self
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key))
            .ok()?;
        let entry = &self.entries[index];
        self.mmap.get(entry.offset..entry.offset + entry.len)
    }
}

fn read_u32(bytes: &[u8], cursor: &mut usize, index: usize) -> Result<u32> {
    let raw = bytes
        .get(*cursor..*cursor + 4)
        .with_context(|| format!("photo entry {index} is truncated"))?;
    *cursor += 4;
    Ok(u32::from_le_bytes(
        raw.try_into().expect("the slice length was checked"),
    ))
}

/// Decode an encoded photo into an egui-ready image.
///
/// Photos are downscaled to a small avatar before upload so browsing a large
/// collection cannot exhaust GPU memory.
pub fn decode_webp(bytes: &[u8]) -> Result<egui::ColorImage> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
        .context("decoding the player photo")?;
    let image = image.thumbnail(160, 160);
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

/// Locate the optional photo pack. `BRETT_PHOTOS` wins, then a source
/// checkout's `assets/photos.bin`, then the conventional Brett Resources
/// location. The pack is never required.
pub fn default_path() -> Option<PathBuf> {
    let env = std::env::var_os("BRETT_PHOTOS")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/photos.bin");
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let candidates = [
        Some(PathBuf::from("assets/photos.bin")),
        Some(manifest),
        executable.as_ref().map(|dir| dir.join("assets/photos.bin")),
        executable.as_ref().map(|dir| dir.join("photos.bin")),
        home_path("Downloads/Brett Resources/Photos/photos.bin"),
    ];
    env.into_iter()
        .chain(candidates.into_iter().flatten())
        .find(|path| path.exists())
}

fn home_path(relative: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_pack(path: &Path, entries: &[(&str, &[u8])]) {
        let mut index = Vec::new();
        let mut data = Vec::new();
        let mut data_offset = HEADER_LEN
            + entries
                .iter()
                .map(|(key, _)| 1 + key.len() + 8)
                .sum::<usize>();
        for (key, bytes) in entries {
            index.push(key.len() as u8);
            index.extend_from_slice(key.as_bytes());
            index.extend_from_slice(&(data_offset as u32).to_le_bytes());
            index.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            data.extend_from_slice(bytes);
            data_offset += bytes.len();
        }

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&(entries.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&index).unwrap();
        file.write_all(&data).unwrap();
    }

    #[test]
    fn reads_sorted_entries_and_canonical_names() {
        let path = std::env::temp_dir().join(format!(
            "brett-photos-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_pack(
            &path,
            &[("carlsen magnus", b"first"), ("hikaru nakamura", b"second")],
        );
        let library = PhotoLibrary::open(&path).unwrap();
        assert_eq!(library.len(), 2);
        assert_eq!(library.bytes("Magnus Carlsen"), Some(&b"first"[..]));
        assert_eq!(library.bytes("Hikaru Nakamura"), Some(&b"second"[..]));
        assert!(library.bytes("Nobody").is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn decodes_a_real_pack_when_present() {
        let Some(path) = default_path() else {
            return;
        };
        let library = PhotoLibrary::open(&path).unwrap();
        assert!(library.len() > 1_000);
        let bytes = library
            .bytes("Carlsen, Magnus")
            .or_else(|| library.bytes("Nakamura, Hikaru"))
            .expect("the photo pack should contain a well-known player");
        let image = decode_webp(bytes).unwrap();
        assert!(image.size[0] > 0 && image.size[1] > 0);
    }

    #[test]
    fn rejects_bad_magic_and_out_of_range_entries() {
        let path = std::env::temp_dir().join(format!(
            "brett-photos-bad-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"NOPE").unwrap();
        assert!(PhotoLibrary::open(&path).is_err());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(3);
        bytes.extend_from_slice(b"abc");
        bytes.extend_from_slice(&999u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(PhotoLibrary::open(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
