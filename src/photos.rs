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

use std::collections::HashMap;
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
    /// Accent-folded key to entry index. The pack and the database can spell
    /// the same player differently (`José` vs `Jose`), so exact lookup alone
    /// would miss many portraits.
    folded: HashMap<String, usize>,
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
        let mut folded = HashMap::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            folded.entry(folded_key(&entry.key)).or_insert(index);
        }

        Ok(Self {
            mmap,
            entries,
            folded,
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
    ///
    /// Exact canonical keys are tried first, then accent-folded keys, then a
    /// conservative short-name match. The last step handles packs that store
    /// `Capablanca, Jose` or `Gukesh D` while the database spells out middle
    /// names. A surname must match exactly and every pack token must be
    /// accounted for, so `Magnus Carlsen Jr` does not borrow Carlsen's photo.
    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        let index = self.matching_entry(name)?;
        self.bytes_at(index)
    }

    /// Resolve an abbreviated PGN name to a `Given Surname` spelling using the
    /// photo pack's canonical full-name keys.
    ///
    /// This is only attempted when the matched key leaves exactly one given
    /// name token, because pack keys are sorted and a longer name cannot be
    /// reordered reliably. The result feeds Wikipedia searches, where
    /// `Svane, R` then finds `Rasmus Svane`.
    pub fn full_name(&self, name: &str) -> Option<String> {
        let index = self.matching_entry(name)?;
        let entry = self.entries.get(index)?;
        let surname = surname(name)?;
        let tokens: Vec<&str> = entry.key.split_whitespace().collect();
        let surname_index = tokens.iter().position(|token| {
            *token == surname || (token.chars().count() == 1 && surname.starts_with(*token))
        })?;
        let given: Vec<&str> = tokens
            .iter()
            .enumerate()
            .filter_map(|(index, token)| (index != surname_index).then_some(*token))
            .collect();
        if given.len() != 1 {
            return None;
        }
        Some(format!(
            "{} {}",
            title_case(given[0]),
            title_case(tokens[surname_index])
        ))
    }

    fn matching_entry(&self, name: &str) -> Option<usize> {
        let key = player_key(name)?;
        if let Ok(index) = self
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key.as_str()))
        {
            return Some(index);
        }
        if let Some(&index) = self.folded.get(&folded_key(name)) {
            return Some(index);
        }
        self.entries
            .iter()
            .position(|entry| entry_matches_name(&entry.key, name))
    }

    /// Find a player's encoded WebP bytes by canonical key.
    pub fn bytes_for_key(&self, key: &str) -> Option<&[u8]> {
        let index = self
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key))
            .ok()?;
        self.bytes_at(index)
    }

    fn bytes_at(&self, index: usize) -> Option<&[u8]> {
        let entry = self.entries.get(index)?;
        self.mmap.get(entry.offset..entry.offset + entry.len)
    }
}

fn folded_key(key: &str) -> String {
    player_key(&deunicode::deunicode(key)).unwrap_or_default()
}

fn title_case(token: &str) -> String {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().chain(chars).collect()
}

fn name_tokens(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| deunicode::deunicode(word).to_lowercase())
        .collect()
}

/// Surname of a display name, honouring `Surname, Given` and `Given Surname`.
fn surname(name: &str) -> Option<String> {
    if let Some((surname, _)) = name.split_once(',') {
        name_tokens(surname).into_iter().next()
    } else {
        name_tokens(name).into_iter().last()
    }
}

fn common_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn entry_matches_name(entry_key: &str, name: &str) -> bool {
    let Some(surname) = surname(name) else {
        return false;
    };
    let display = name_tokens(name);
    let entry: Vec<&str> = entry_key.split_whitespace().collect();
    let Some(surname_index) = entry
        .iter()
        .position(|token| *token == surname || (token.len() == 1 && surname.starts_with(token)))
    else {
        return false;
    };

    let mut matched_other = false;
    for (index, token) in entry.iter().enumerate() {
        if index == surname_index {
            continue;
        }
        let matched = display.iter().any(|display_token| {
            *display_token == *token
                || (token.len() == 1 && display_token.starts_with(token))
                || (display_token.len() == 1 && token.starts_with(display_token))
                || common_prefix(display_token, token) >= 3
        });
        if !matched {
            return false;
        }
        matched_other = true;
    }
    matched_other
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
    fn matches_folded_and_short_pack_keys_without_false_positives() {
        let path = std::env::temp_dir().join(format!(
            "brett-photos-folded-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_pack(
            &path,
            &[
                ("capablanca jose", b"capablanca"),
                ("carlsen magnus", b"carlsen"),
                ("d gukesh", b"gukesh"),
                ("nakamura hikaru", b"nakamura"),
            ],
        );
        let library = PhotoLibrary::open(&path).unwrap();
        assert_eq!(
            library.bytes("José Raúl Capablanca"),
            Some(&b"capablanca"[..])
        );
        assert_eq!(
            library.bytes("Capablanca, Jose Raul"),
            Some(&b"capablanca"[..])
        );
        assert_eq!(library.bytes("Gukesh Dommaraju"), Some(&b"gukesh"[..]));
        assert_eq!(library.bytes("D Gukesh"), Some(&b"gukesh"[..]));
        assert_eq!(library.bytes("Magnus Carlsen"), Some(&b"carlsen"[..]));
        assert_eq!(library.bytes("Magnus Carlsen Jr"), None);
        assert_eq!(library.bytes("Ding Liren Fan"), None);
        assert_eq!(library.bytes("Hikaru Nakamura"), Some(&b"nakamura"[..]));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn abbreviated_names_resolve_to_the_pack_full_name() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-photos-full-name-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_pack(
            &path,
            &[
                ("bacrot etienne", b"bacrot"),
                ("buhmann rainer", b"buhmann"),
                ("carlsen magnus", b"carlsen"),
                ("rasmus svane", b"svane"),
            ],
        );
        let library = PhotoLibrary::open(&path).unwrap();
        assert_eq!(
            library.full_name("Bacrot, E").as_deref(),
            Some("Etienne Bacrot")
        );
        assert_eq!(
            library.full_name("Buhmann,R").as_deref(),
            Some("Rainer Buhmann")
        );
        assert_eq!(
            library.full_name("Svane, R").as_deref(),
            Some("Rasmus Svane")
        );
        assert_eq!(
            library.full_name("Carlsen, Magnus").as_deref(),
            Some("Magnus Carlsen")
        );
        assert_eq!(library.full_name("Nobody, X"), None);
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
