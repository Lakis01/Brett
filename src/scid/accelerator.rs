//! `<stem>.brettacc` CSR sidecar for SCID databases.
//!
//! The sidecar is keyed by the `.si4`/`.sn4`/`.sg4` size and modification
//! time: when any of the three files changes the sidecar is treated as stale
//! and rebuilt. For every player id and event id it stores the slice of game
//! indices (positions in [`ScidLibrary`](super::ScidLibrary)'s game list)
//! plus pre-aggregated `(white_games, black_games, wins, draws, losses,
//! max_elo, first_year, last_year)` from that player's perspective. The CSR
//! tables cost roughly 12 bytes of indices per game plus a small aggregate
//! row, targeted at under 15% of the `.sg4` size.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Entry, ScidLibrary};
use crate::library::SourceMeta;

const MAGIC: &[u8; 8] = b"BRETTAC\x01";
const VERSION: u32 = 1;

/// Pre-aggregated record for one player, from that player's perspective.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerStats {
    pub white_games: u32,
    pub black_games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub max_elo: u16,
    pub first_year: u16,
    pub last_year: u16,
}

/// CSR sidecar: per player id and per event id, the game indices plus
/// per-player aggregates.
#[derive(Clone, Debug)]
pub struct ScidAccelerator {
    path: PathBuf,
    n_games: u32,
    n_players: u32,
    n_events: u32,
    player_starts: Vec<u32>,
    player_indices: Vec<u32>,
    player_stats: Vec<PlayerStats>,
    event_starts: Vec<u32>,
    event_indices: Vec<u32>,
}

impl ScidAccelerator {
    /// Companion sidecar path for a `.si4` index path.
    pub fn path_for(index_path: &Path) -> PathBuf {
        index_path.with_extension("brettacc")
    }

    fn fingerprint_parts(library: &ScidLibrary) -> Result<[(SourceMeta, PathBuf); 3]> {
        let si_stem = library.path.clone();
        let sn = library.name_path.clone();
        let sg = library.game_path.clone();
        Ok([
            (SourceMeta::read(&si_stem)?, si_stem),
            (SourceMeta::read(&sn)?, sn),
            (SourceMeta::read(&sg)?, sg),
        ])
    }

    /// Build the accelerator from a loaded library's index entries.
    pub fn build(library: &ScidLibrary) -> Result<Self> {
        let entries: &[Entry] = &library.entries;
        let n_games = entries.len();
        let mut max_player = 0u32;
        let mut max_event = 0u32;
        for entry in entries {
            max_player = max_player.max(entry.white).max(entry.black);
            max_event = max_event.max(entry.event);
        }
        let n_players = if n_games == 0 { 0 } else { max_player + 1 };
        let n_events = if n_games == 0 { 0 } else { max_event + 1 };

        let mut player_refs: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut event_refs: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut stats: HashMap<u32, PlayerStats> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let index = index as u32;
            player_refs.entry(entry.white).or_default().push(index);
            player_refs.entry(entry.black).or_default().push(index);
            event_refs.entry(entry.event).or_default().push(index);
            for (id, own_color) in [(entry.white, true), (entry.black, false)] {
                let row = stats.entry(id).or_default();
                let (own_elo, result) = if own_color {
                    row.white_games += 1;
                    (entry.white_elo, result_for(true, entry.result))
                } else {
                    row.black_games += 1;
                    (entry.black_elo, result_for(false, entry.result))
                };
                match result {
                    1 => row.wins += 1,
                    2 => row.draws += 1,
                    3 => row.losses += 1,
                    _ => {}
                }
                row.max_elo = row.max_elo.max(own_elo);
                let year = scid_year(entry.date);
                if year != 0 {
                    if row.first_year == 0 || year < row.first_year {
                        row.first_year = year;
                    }
                    row.last_year = row.last_year.max(year);
                }
            }
        }

        let (player_starts, player_indices) = csr(&player_refs, n_players, n_games);
        let (event_starts, event_indices) = csr(&event_refs, n_events, n_games);
        let player_stats = (0..n_players)
            .map(|id| stats.get(&id).copied().unwrap_or_default())
            .collect();

        Ok(Self {
            path: Self::path_for(&library.path),
            n_games: n_games as u32,
            n_players,
            n_events,
            player_starts,
            player_indices,
            player_stats,
            event_starts,
            event_indices,
        })
    }

    /// Build and write the sidecar beside the index.
    pub fn build_to(library: &ScidLibrary, path: &Path) -> Result<()> {
        let acc = Self::build(library)?;
        let parts = Self::fingerprint_parts(library)?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(acc.n_games).to_le_bytes());
        for (meta, _) in &parts {
            out.extend_from_slice(&meta.len.to_le_bytes());
            out.extend_from_slice(&meta.mtime_secs.to_le_bytes());
            out.extend_from_slice(&meta.mtime_nanos.to_le_bytes());
        }
        out.extend_from_slice(&acc.n_players.to_le_bytes());
        out.extend_from_slice(&acc.n_events.to_le_bytes());
        for start in &acc.player_starts {
            out.extend_from_slice(&start.to_le_bytes());
        }
        let player_blob = encode_stream(&acc.player_starts, &acc.player_indices);
        out.extend_from_slice(&(player_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&player_blob);
        for row in &acc.player_stats {
            out.extend_from_slice(&row.white_games.to_le_bytes());
            out.extend_from_slice(&row.black_games.to_le_bytes());
            out.extend_from_slice(&row.wins.to_le_bytes());
            out.extend_from_slice(&row.draws.to_le_bytes());
            out.extend_from_slice(&row.losses.to_le_bytes());
            out.extend_from_slice(&row.max_elo.to_le_bytes());
            out.extend_from_slice(&row.first_year.to_le_bytes());
            out.extend_from_slice(&row.last_year.to_le_bytes());
        }
        for start in &acc.event_starts {
            out.extend_from_slice(&start.to_le_bytes());
        }
        let event_blob = encode_stream(&acc.event_starts, &acc.event_indices);
        out.extend_from_slice(&(event_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&event_blob);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, &out)
            .with_context(|| format!("writing SCID accelerator {}", path.display()))?;
        Ok(())
    }

    /// Load the sidecar for an index path, returning `Ok(None)` when the
    /// file is missing or its size/mtime fingerprint no longer matches the
    /// `.si4`/`.sn4`/`.sg4` trio.
    pub fn load(index_path: &Path) -> Result<Option<Self>> {
        let path = Self::path_for(index_path);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("reading SCID accelerator {}", path.display()))?;
        let Some((acc, fingerprint)) = Self::parse(&path, &bytes)? else {
            return Ok(None);
        };
        // Validate against the current trio.
        let suffix = index_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("si4");
        let stem = index_path.with_extension("");
        let sibling = |ext: &str| {
            let mut name = stem.as_os_str().to_owned();
            name.push(ext);
            PathBuf::from(name)
        };
        let _ = suffix;
        let trio = [sibling(".si4"), sibling(".sn4"), sibling(".sg4")];
        // Also accept the v3 trio.
        let trios = if trio.iter().all(|p| p.is_file()) {
            trio.to_vec()
        } else {
            [sibling(".si3"), sibling(".sn3"), sibling(".sg3")].to_vec()
        };
        let current: Vec<SourceMeta> = trios
            .iter()
            .map(|candidate| SourceMeta::read(candidate))
            .collect::<Result<_>>()?;
        if current != fingerprint {
            return Ok(None);
        }
        Ok(Some(acc))
    }

    fn parse(path: &Path, bytes: &[u8]) -> Result<Option<(Self, Vec<SourceMeta>)>> {
        let mut cursor = 0usize;
        if bytes.len() < 8 + 4 + 4 + 3 * 20 + 8 {
            return Ok(None);
        }
        if take_field(path, bytes, &mut cursor, 8)? != MAGIC {
            return Ok(None);
        }
        let version = le_u32(take_field(path, bytes, &mut cursor, 4)?);
        if version != VERSION {
            return Ok(None);
        }
        let n_games = le_u32(take_field(path, bytes, &mut cursor, 4)?);
        let mut fingerprint = Vec::with_capacity(3);
        for _ in 0..3 {
            let len = le_u64(take_field(path, bytes, &mut cursor, 8)?);
            let secs = le_u64(take_field(path, bytes, &mut cursor, 8)?);
            let nanos = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            fingerprint.push(SourceMeta {
                len,
                mtime_secs: secs,
                mtime_nanos: nanos,
            });
        }
        let n_players = le_u32(take_field(path, bytes, &mut cursor, 4)?) as usize;
        let n_events = le_u32(take_field(path, bytes, &mut cursor, 4)?) as usize;
        if n_players > 1_100_000 || n_events > 600_000 {
            return Ok(None);
        }
        let mut player_starts = Vec::with_capacity(n_players + 1);
        for _ in 0..n_players + 1 {
            player_starts.push(le_u32(take_field(path, bytes, &mut cursor, 4)?));
        }
        let player_total = player_starts.last().copied().unwrap_or(0) as usize;
        if player_total > (n_games as usize).saturating_mul(2) {
            return Ok(None);
        }
        let player_blob_len = le_u32(take_field(path, bytes, &mut cursor, 4)?) as usize;
        let player_blob = take_field(path, bytes, &mut cursor, player_blob_len)?;
        let player_indices = decode_stream(&player_starts, player_blob).ok_or_else(|| {
            anyhow::anyhow!(
                "SCID accelerator {} has a damaged player stream",
                path.display()
            )
        })?;
        if player_indices.len() != player_total {
            return Ok(None);
        }
        let mut player_stats = Vec::with_capacity(n_players);
        for _ in 0..n_players {
            let white_games = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            let black_games = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            let wins = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            let draws = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            let losses = le_u32(take_field(path, bytes, &mut cursor, 4)?);
            let max_elo = le_u16(take_field(path, bytes, &mut cursor, 2)?);
            let first_year = le_u16(take_field(path, bytes, &mut cursor, 2)?);
            let last_year = le_u16(take_field(path, bytes, &mut cursor, 2)?);
            player_stats.push(PlayerStats {
                white_games,
                black_games,
                wins,
                draws,
                losses,
                max_elo,
                first_year,
                last_year,
            });
        }
        let mut event_starts = Vec::with_capacity(n_events + 1);
        for _ in 0..n_events + 1 {
            event_starts.push(le_u32(take_field(path, bytes, &mut cursor, 4)?));
        }
        let event_total = event_starts.last().copied().unwrap_or(0) as usize;
        if event_total != n_games as usize {
            // Event slices partition the game list exactly.
            return Ok(None);
        }
        let event_blob_len = le_u32(take_field(path, bytes, &mut cursor, 4)?) as usize;
        let event_blob = take_field(path, bytes, &mut cursor, event_blob_len)?;
        let event_indices = decode_stream(&event_starts, event_blob).ok_or_else(|| {
            anyhow::anyhow!(
                "SCID accelerator {} has a damaged event stream",
                path.display()
            )
        })?;
        if event_indices.len() != event_total {
            return Ok(None);
        }
        if cursor != bytes.len() {
            return Ok(None);
        }
        Ok(Some((
            Self {
                path: path.to_path_buf(),
                n_games,
                n_players: n_players as u32,
                n_events: n_events as u32,
                player_starts,
                player_indices,
                player_stats,
                event_starts,
                event_indices,
            },
            fingerprint,
        )))
    }

    pub fn n_games(&self) -> u32 {
        self.n_games
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn n_players(&self) -> u32 {
        self.n_players
    }

    pub fn n_events(&self) -> u32 {
        self.n_events
    }

    /// Game indices for one player id.
    pub fn player_games(&self, id: u32) -> &[u32] {
        slice_of(&self.player_starts, &self.player_indices, id)
    }

    /// Game indices for one event id.
    pub fn event_games(&self, id: u32) -> &[u32] {
        slice_of(&self.event_starts, &self.event_indices, id)
    }

    pub fn player_stats(&self, id: u32) -> Option<&PlayerStats> {
        self.player_stats.get(id as usize)
    }
}

/// Encode sorted per-id slices as first-value plus delta LEB128 varints.
/// The API still exposes plain `u32` slices; only the on-disk stream is
/// compressed so the sidecar stays far below the `.sg4` size.
fn encode_stream(starts: &[u32], indices: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for id in 0..starts.len().saturating_sub(1) {
        let (start, end) = (starts[id] as usize, starts[id + 1] as usize);
        if start >= end {
            continue;
        }
        let mut previous = indices[start];
        put_varint(&mut out, previous);
        for &value in &indices[start + 1..end] {
            put_varint(&mut out, value.wrapping_sub(previous));
            previous = value;
        }
    }
    out
}

fn decode_stream(starts: &[u32], blob: &[u8]) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for id in 0..starts.len().saturating_sub(1) {
        let count = starts[id + 1].wrapping_sub(starts[id]) as usize;
        if count == 0 {
            continue;
        }
        let mut previous = get_varint(blob, &mut cursor)?;
        out.push(previous);
        for _ in 1..count {
            let delta = get_varint(blob, &mut cursor)?;
            previous = previous.wrapping_add(delta);
            out.push(previous);
        }
    }
    (cursor == blob.len()).then_some(out)
}

fn put_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn get_varint(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        value |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn take_field<'a>(
    path: &Path,
    bytes: &'a [u8],
    cursor: &mut usize,
    len: usize,
) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .with_context(|| format!("SCID accelerator {} size overflow", path.display()))?;
    let slice = bytes
        .get(*cursor..end)
        .with_context(|| format!("SCID accelerator {} is truncated", path.display()))?;
    *cursor = end;
    Ok(slice)
}

fn slice_of<'a>(starts: &'a [u32], indices: &'a [u32], id: u32) -> &'a [u32] {
    let id = id as usize;
    if id + 1 >= starts.len() {
        return &[];
    }
    let start = starts[id] as usize;
    let end = starts[id + 1] as usize;
    if start > end || end > indices.len() {
        return &[];
    }
    &indices[start..end]
}

fn csr(refs: &HashMap<u32, Vec<u32>>, width: u32, n_games: usize) -> (Vec<u32>, Vec<u32>) {
    let mut starts = Vec::with_capacity(width as usize + 1);
    let mut indices = Vec::new();
    for id in 0..width {
        starts.push(indices.len() as u32);
        if let Some(list) = refs.get(&id) {
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            indices.extend(sorted);
        }
    }
    starts.push(indices.len() as u32);
    // Empty database: width 0 still yields one zero start.
    if starts.is_empty() {
        starts.push(0);
    }
    let _ = n_games;
    (starts, indices)
}

/// 1 = win, 2 = draw, 3 = loss from the perspective of `white_side`.
fn result_for(white_side: bool, result: u8) -> u8 {
    match (white_side, result) {
        (true, 1) | (false, 2) => 1,
        (_, 3) => 2,
        (true, 2) | (false, 1) => 3,
        _ => 0,
    }
}

fn scid_year(date: u32) -> u16 {
    if date == 0 {
        return 0;
    }
    ((date >> 9) & 0x7FFF) as u16
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;
    use crate::scid::writer::write_database;

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "brett-scidacc-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("test.si4");
            Self { dir, path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn game_pgn(white: &str, black: &str, event: &str, result: &str, date: &str) -> String {
        format!(
            "[Event \"{event}\"]\n[Site \"Testville\"]\n[Date \"{date}\"]\n[Round \"1\"]\n\
             [White \"{white}\"]\n[Black \"{black}\"]\n[Result \"{result}\"]\n\
             [WhiteElo \"2700\"]\n[BlackElo \"2650\"]\n\n\
             1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 \
             7. Bb3 d6 8. c3 O-O 9. h3 Na5 10. Bc2 c5 {result}\n"
        )
    }

    #[test]
    fn csr_lookup_matches_index_and_aggregates() {
        // Gukesh-like synthetic set: one player across several events.
        let db = TempDb::new("csr");
        let pgns = [
            game_pgn(
                "Gukesh, D",
                "Erigaisi, A",
                "Candidates",
                "1-0",
                "2024.04.01",
            ),
            game_pgn(
                "Nepomniachtchi, I",
                "Gukesh, D",
                "Candidates",
                "1/2-1/2",
                "2024.04.02",
            ),
            game_pgn("Gukesh, D", "Nakamura, H", "Olympiad", "0-1", "2024.09.11"),
            game_pgn("Gukesh, D", "Carlsen, M", "Olympiad", "1-0", "2024.09.12"),
        ];
        let parsed: Vec<crate::domain::Game> = pgns
            .iter()
            .flat_map(|pgn| parse_pgn(pgn.as_bytes()).unwrap())
            .collect();
        write_database(&db.path, "accelerator test", parsed.iter().cloned().map(Ok)).unwrap();
        let library = ScidLibrary::open(&db.path).unwrap();
        let sidecar = ScidAccelerator::path_for(&db.path);
        ScidAccelerator::build_to(&library, &sidecar).unwrap();

        let acc = ScidAccelerator::load(&db.path)
            .unwrap()
            .expect("sidecar loads");
        assert_eq!(acc.n_games(), 4);

        // Find Gukesh's player id through the library headers.
        let gukesh_id = library
            .entries
            .iter()
            .find(|e| library.names.players[e.white as usize] == "Gukesh, D")
            .map(|e| e.white)
            .unwrap();
        let games = acc.player_games(gukesh_id);
        assert_eq!(games.len(), 4);
        let stats = acc.player_stats(gukesh_id).unwrap();
        assert_eq!(stats.white_games, 3);
        assert_eq!(stats.black_games, 1);
        assert_eq!(stats.wins, 2);
        assert_eq!(stats.draws, 1);
        assert_eq!(stats.losses, 1);
        assert_eq!(stats.max_elo, 2700);
        assert_eq!(stats.first_year, 2024);
        assert_eq!(stats.last_year, 2024);

        // Event slice for "Candidates" holds exactly games 0 and 1.
        let event_id = library.entries.first().map(|e| e.event).unwrap();
        assert_eq!(acc.event_games(event_id), &[0, 1]);
    }

    #[test]
    fn sidecar_stays_far_smaller_than_the_game_file() {
        // Volume test for the <0.15x `.sg4` size target: 600 realistic
        // games across 12 players and 4 events.
        let db = TempDb::new("volume");
        let line = "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 \
                    7. Bb3 d6 8. c3 O-O 9. h3 Na5 10. Bc2 c5 11. d4 Qc7 12. Nbd2 cxd4 \
                    13. cxd4 Nc6 14. Nb3 a5 15. Be3 a4 16. Nbd2 a3 17. Rc1 axb2 \
                    18. Rxc6 Qxc6 19. Bxb2 Bb7 20. Qd3 Rfc8";
        let mut pgns = Vec::new();
        for i in 0..600 {
            let white = format!("Player, W{:02}", i % 12);
            let black = format!("Player, B{:02}", (i * 7) % 12);
            let event = format!("Event {:02}", i % 4);
            let result = ["1-0", "0-1", "1/2-1/2"][i % 3];
            let date = format!("20{:02}.{:02}.{:02}", 10 + i % 15, 1 + i % 12, 1 + i % 28);
            pgns.push(format!(
                "[Event \"{event}\"]\n[Site \"Testville\"]\n[Date \"{date}\"]\n[Round \"{}\"]\n\
                 [White \"{white}\"]\n[Black \"{black}\"]\n[Result \"{result}\"]\n\
                 [WhiteElo \"{}\"]\n[BlackElo \"{}\"]\n\n{line} {result}\n",
                1 + i % 9,
                2400 + i % 300,
                2400 + (i * 13) % 300,
            ));
        }
        let parsed: Vec<crate::domain::Game> = pgns
            .iter()
            .flat_map(|pgn| parse_pgn(pgn.as_bytes()).unwrap())
            .collect();
        assert_eq!(parsed.len(), 600);
        write_database(&db.path, "volume test", parsed.iter().cloned().map(Ok)).unwrap();
        let library = ScidLibrary::open(&db.path).unwrap();
        let sidecar = ScidAccelerator::path_for(&db.path);
        ScidAccelerator::build_to(&library, &sidecar).unwrap();

        let sg_len = std::fs::metadata(db.path.with_extension("sg4"))
            .unwrap()
            .len();
        let acc_len = std::fs::metadata(&sidecar).unwrap().len();
        assert!(
            (acc_len as f64) < 0.15 * (sg_len as f64),
            "sidecar {acc_len} vs sg4 {sg_len}"
        );
        let acc = ScidAccelerator::load(&db.path)
            .unwrap()
            .expect("sidecar loads");
        assert_eq!(acc.n_games(), 600);
    }

    #[test]
    fn stale_fingerprint_is_rejected() {
        let db = TempDb::new("stale");
        let pgns = [game_pgn("A, A", "B, B", "E", "1-0", "2024.01.01")];
        let parsed: Vec<crate::domain::Game> = pgns
            .iter()
            .flat_map(|pgn| parse_pgn(pgn.as_bytes()).unwrap())
            .collect();
        write_database(&db.path, "stale test", parsed.iter().cloned().map(Ok)).unwrap();
        let library = ScidLibrary::open(&db.path).unwrap();
        let sidecar = ScidAccelerator::path_for(&db.path);
        ScidAccelerator::build_to(&library, &sidecar).unwrap();
        assert!(ScidAccelerator::load(&db.path).unwrap().is_some());
        // Touching the game file invalidates the sidecar.
        let sg = db.path.with_extension("sg4");
        let mut bytes = std::fs::read(&sg).unwrap();
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(&sg, &bytes).unwrap();
        assert!(ScidAccelerator::load(&db.path).unwrap().is_none());
    }
}
