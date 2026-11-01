use super::*;
use crate::domain::Game;
use crate::twocbh::TwoCbhFile;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brett-2cbh-writer-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn writes_a_readable_empty_database() {
    let dir = TempDir::new();
    let path = dir.path().join("empty.2cbh");
    let report = write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    assert_eq!(report.games, 0);
    assert!(report.bytes > 0);

    let file = TwoCbhFile::open(&path).unwrap();
    assert_eq!(file.slot_count(), 1);
    assert_eq!(file.game_count(), 0);

    let cbh = std::fs::read(&path).unwrap();
    assert_eq!(cbh.len(), 192);
    assert_eq!(cbh[8], 0x26);
    assert_eq!(u16::from_le_bytes([cbh[0x0a], cbh[0x0b]]), 192);
    assert_eq!(u16::from_le_bytes([cbh[0x0c], cbh[0x0d]]), 0x0500);
    assert_eq!(u32::from_le_bytes(cbh[0x10..0x14].try_into().unwrap()), 1);

    let cbg = std::fs::read(path.with_extension("2cbg")).unwrap();
    assert_eq!(cbg.len(), 12);
    assert_eq!(u64::from_le_bytes(cbg[0..8].try_into().unwrap()), 12);
    assert_eq!(u16::from_le_bytes([cbg[8], cbg[9]]), 12);
    assert_eq!(u16::from_le_bytes([cbg[10], cbg[11]]), 0x0500);

    let cba = std::fs::read(path.with_extension("2cba")).unwrap();
    assert_eq!(cba.len(), 12);
    assert_eq!(u16::from_le_bytes([cba[10], cba[11]]), 0);

    let lid = std::fs::read(path.with_extension("2lid")).unwrap();
    assert_eq!(lid.len(), 0xB8 + 0x108A);
    let be = |at: usize| u32::from_be_bytes(lid[at..at + 4].try_into().unwrap());
    assert_eq!(be(0x00), 0xB8);
    assert_eq!(be(0x04), 6);
    assert_eq!(be(0x08), 0x400);
    assert_eq!(be(0x0c), 0);
    assert_eq!(be(0x10), 2);
    assert_eq!(be(0x14), u32::MAX);
    assert_eq!(be(0x18), u32::MAX);
    assert_eq!(be(0x1c), 0x460);
    assert_eq!(be(0x24), 0);
    assert_eq!(be(0x38), 2);
    assert_eq!(be(0x44), 0x400);
    assert_eq!(be(0x74), 1);
    assert_eq!(be(0x80), u32::MAX);
}

#[test]
fn normalizes_companion_extensions_and_rejects_other_targets() {
    let dir = TempDir::new();
    let path = dir.path().join("db.2cbg");
    write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    assert!(dir.path().join("db.2cbh").exists());
    assert!(dir.path().join("db.2cbg").exists());

    let err = write_database(&dir.path().join("db.txt"), Vec::<Result<Game>>::new()).unwrap_err();
    assert!(format!("{err:#}").contains("must end in .2cbh"));

    let err = write_database(
        &dir.path().join("missing/db.2cbh"),
        Vec::<Result<Game>>::new(),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("does not exist"));
}

#[test]
fn removes_stale_derived_indexes_and_preserves_unrelated_files() {
    let dir = TempDir::new();
    let path = dir.path().join("replace.2cbh");
    write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    std::fs::write(path.with_extension("2lcd"), b"stale").unwrap();
    std::fs::write(path.with_extension("2lgd"), b"stale").unwrap();
    std::fs::write(path.with_extension("ini"), b"keep").unwrap();

    write_database(&path, Vec::<Result<Game>>::new()).unwrap();

    assert!(!path.with_extension("2lcd").exists());
    assert!(!path.with_extension("2lgd").exists());
    assert_eq!(std::fs::read(path.with_extension("ini")).unwrap(), b"keep");
}
