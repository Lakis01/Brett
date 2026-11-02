//! Local persistent analysis cache.
//!
//! Stores engine analysis (engine name, depth, score, multipv, PV moves)
//! in SQLite with WAL mode. Supports coexistence of engine families (Stockfish,
//! Leela, Generic) and fine-grained replacement heuristics.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineFamily {
    Stockfish,
    Leela,
    Generic,
}

impl std::str::FromStr for EngineFamily {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "stockfish" => Self::Stockfish,
            "leela" | "lc0" => Self::Leela,
            _ => Self::Generic,
        })
    }
}

impl EngineFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stockfish => "stockfish",
            Self::Leela => "leela",
            Self::Generic => "generic",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedEngineVersion {
    pub family: EngineFamily,
    pub major: Option<u32>,
    pub minor: Option<u32>,
    pub dev_date: Option<u32>, // YYYYMMDD
    pub baseline_elo: f64,
}

pub fn parse_engine_identity(name: &str) -> ParsedEngineVersion {
    let lower = name.trim().to_ascii_lowercase();
    let family = if lower.contains("stockfish") {
        EngineFamily::Stockfish
    } else if lower.contains("leela") || lower.contains("lc0") {
        EngineFamily::Leela
    } else {
        EngineFamily::Generic
    };

    let mut major = None;
    let mut minor = None;
    let mut dev_date = None;

    if family == EngineFamily::Stockfish {
        // Look for dev date, e.g. dev-20241018
        if let Some(pos) = lower.find("dev-") {
            let digits: String = lower[pos + 4..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if digits.len() == 8
                && let Ok(d) = digits.parse::<u32>()
            {
                dev_date = Some(d);
            }
        }

        // Look for major and minor version numbers
        let tokens: Vec<&str> = name.split_whitespace().collect();
        for token in tokens {
            let parts: Vec<&str> = token.split('.').collect();
            if let Some(first) = parts.first()
                && let Ok(maj) = first.parse::<u32>()
                && (10..=30).contains(&maj)
            {
                major = Some(maj);
                if parts.len() > 1
                    && let Ok(min) = parts[1].parse::<u32>()
                {
                    minor = Some(min);
                }
                break;
            }
        }
    }

    let baseline_elo = match family {
        EngineFamily::Stockfish => {
            if let Some(date) = dev_date {
                // Approximate scaling: Stockfish 17 was released Sept 2024 (~20240901)
                let day_offset = (date as i64 - 20240901).max(0) as f64;
                (3830.0 + day_offset * 0.25).min(4100.0)
            } else {
                match (major, minor) {
                    (Some(17), _) => 3830.0,
                    (Some(16), Some(1)) => 3790.0,
                    (Some(16), _) => 3760.0,
                    (Some(15), Some(1)) => 3720.0,
                    (Some(15), _) => 3690.0,
                    (Some(14), Some(1)) => 3650.0,
                    (Some(14), _) => 3620.0,
                    (Some(13), _) => 3570.0,
                    (Some(12), _) => 3520.0,
                    (Some(11), _) => 3450.0,
                    (Some(m), _) if m > 17 => 3830.0 + (m - 17) as f64 * 40.0,
                    _ => 3400.0,
                }
            }
        }
        EngineFamily::Leela => 3650.0,
        EngineFamily::Generic => 3400.0,
    };

    ParsedEngineVersion {
        family,
        major,
        minor,
        dev_date,
        baseline_elo,
    }
}

pub fn compute_search_strength(depth: u32) -> f64 {
    if depth == 0 {
        return 0.0;
    }
    let mut strength = 0.0;
    let p1 = depth.min(18);
    strength += p1 as f64 * 50.0;
    if depth > 18 {
        let p2 = (depth - 18).min(12);
        strength += p2 as f64 * 35.0;
    }
    if depth > 30 {
        let p3 = depth - 30;
        strength += p3 as f64 * 20.0;
    }
    strength
}

pub fn compute_quality_rating(
    _family: EngineFamily,
    baseline_elo: f64,
    depth: u32,
    seldepth: Option<u32>,
    nodes: Option<u64>,
    score_mate: Option<i32>,
) -> f64 {
    let mut q = baseline_elo + compute_search_strength(depth);
    if let Some(sel) = seldepth
        && sel > depth
    {
        q += (sel - depth) as f64 * 0.5;
    }
    if let Some(n) = nodes
        && n > 100_000
    {
        q += (n as f64 / 100_000.0).log10() * 3.0;
    }
    if let Some(mate) = score_mate {
        q += 20_000.0 - mate.unsigned_abs() as f64 * 10.0;
    }
    q
}

pub fn should_supersede(
    incoming: &CachedAnalysis,
    existing: &CachedAnalysis,
    heuristic_supersede: bool,
) -> bool {
    if incoming.engine_family != existing.engine_family {
        return false;
    }
    if incoming.engine_name == existing.engine_name {
        return incoming.depth >= existing.depth;
    }
    if !heuristic_supersede {
        return false;
    }
    // With heuristic superseding active, require incoming quality to exceed existing plus a 15 Elo margin
    incoming.quality_score > existing.quality_score + 15.0
}

/// Translate Leela centipawns to Stockfish-equivalent centipawns.
///
/// Leela's UCI evaluation scale is compressed relative to Stockfish NNUE:
/// +50 cp in Leela corresponds to ~+78 cp in Stockfish, and +100 cp corresponds
/// to ~+165 cp (reflecting decisive winning chances).
pub fn leela_to_stockfish_cp(cp: i32) -> i32 {
    if cp == 0 {
        return 0;
    }
    let sign = cp.signum();
    let abs = cp.abs() as f64;
    let scaled = abs * 1.45 + (abs / 100.0).powi(2) * 20.0;
    sign * scaled.round() as i32
}

#[derive(Clone, Debug, PartialEq)]
pub struct CachedAnalysis {
    pub pos_hash: u64,
    pub fen: String,
    pub engine_family: EngineFamily,
    pub engine_name: String,
    pub multipv: usize,
    pub depth: u32,
    pub seldepth: Option<u32>,
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub nodes: Option<u64>,
    pub time_ms: Option<u64>,
    pub nps: Option<u64>,
    pub pv: Vec<String>,
    pub quality_score: f64,
    pub analyzed_at: u64,
}

pub struct AnalysisDb {
    conn: Connection,
}

impl AnalysisDb {
    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("opening analysis database at {}", path.display()))?;

        // WAL mode enables fast non-blocking reads and concurrent transactions.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS position_analysis (
                pos_hash INTEGER NOT NULL,
                fen TEXT NOT NULL,
                engine_family TEXT NOT NULL,
                engine_name TEXT NOT NULL,
                multipv INTEGER NOT NULL,
                depth INTEGER NOT NULL,
                seldepth INTEGER,
                score_cp INTEGER,
                score_mate INTEGER,
                nodes INTEGER,
                time_ms INTEGER,
                nps INTEGER,
                pv TEXT NOT NULL,
                quality_score REAL NOT NULL,
                analyzed_at INTEGER NOT NULL,
                PRIMARY KEY (pos_hash, engine_family, multipv, engine_name)
            );
            CREATE INDEX IF NOT EXISTS idx_pos_family ON position_analysis(pos_hash, engine_family);",
        )?;

        Ok(Self { conn })
    }

    pub fn default_path() -> PathBuf {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(dir).join("brett").join("analysis.db");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("brett")
                .join("analysis.db");
        }
        std::env::temp_dir().join("brett-analysis.db")
    }

    pub fn open_default() -> Result<Self> {
        Self::open_at(&Self::default_path())
    }

    pub fn get_position(&self, pos_hash: u64) -> Result<Vec<CachedAnalysis>> {
        let mut stmt = self.conn.prepare(
            "SELECT pos_hash, fen, engine_family, engine_name, multipv, depth, seldepth,
                    score_cp, score_mate, nodes, time_ms, nps, pv, quality_score, analyzed_at
             FROM position_analysis
             WHERE pos_hash = ?1
             ORDER BY multipv ASC, quality_score DESC",
        )?;

        let hash_i64 = pos_hash as i64;
        let rows = stmt.query_map(params![hash_i64], |row| {
            let pos_hash_val: i64 = row.get(0)?;
            let fen: String = row.get(1)?;
            let family_str: String = row.get(2)?;
            let engine_name: String = row.get(3)?;
            let multipv: usize = row.get(4)?;
            let depth: u32 = row.get(5)?;
            let seldepth: Option<u32> = row.get(6)?;
            let score_cp: Option<i32> = row.get(7)?;
            let score_mate: Option<i32> = row.get(8)?;
            let nodes: Option<i64> = row.get(9)?;
            let time_ms: Option<i64> = row.get(10)?;
            let nps: Option<i64> = row.get(11)?;
            let pv_str: String = row.get(12)?;
            let quality_score: f64 = row.get(13)?;
            let analyzed_at: i64 = row.get(14)?;

            let pv = pv_str.split_whitespace().map(str::to_string).collect();

            Ok(CachedAnalysis {
                pos_hash: pos_hash_val as u64,
                fen,
                engine_family: family_str.parse().unwrap_or(EngineFamily::Generic),
                engine_name,
                multipv,
                depth,
                seldepth,
                score_cp,
                score_mate,
                nodes: nodes.map(|n| n as u64),
                time_ms: time_ms.map(|t| t as u64),
                nps: nps.map(|n| n as u64),
                pv,
                quality_score,
                analyzed_at: analyzed_at as u64,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    pub fn store(&mut self, analysis: &CachedAnalysis, supersede_older: bool) -> Result<bool> {
        let hash_i64 = analysis.pos_hash as i64;
        let family_str = analysis.engine_family.as_str();

        // Check if an existing entry exists for (pos_hash, engine_family, multipv)
        let mut check_stmt = self.conn.prepare(
            "SELECT engine_name, depth, quality_score FROM position_analysis
             WHERE pos_hash = ?1 AND engine_family = ?2 AND multipv = ?3",
        )?;

        let existing =
            check_stmt.query_row(params![hash_i64, family_str, analysis.multipv], |row| {
                let name: String = row.get(0)?;
                let depth: u32 = row.get(1)?;
                let quality: f64 = row.get(2)?;
                Ok((name, depth, quality))
            });

        match existing {
            Ok((existing_name, existing_depth, existing_quality)) => {
                let existing_mock = CachedAnalysis {
                    pos_hash: analysis.pos_hash,
                    fen: analysis.fen.clone(),
                    engine_family: analysis.engine_family,
                    engine_name: existing_name.clone(),
                    multipv: analysis.multipv,
                    depth: existing_depth,
                    seldepth: None,
                    score_cp: None,
                    score_mate: None,
                    nodes: None,
                    time_ms: None,
                    nps: None,
                    pv: Vec::new(),
                    quality_score: existing_quality,
                    analyzed_at: 0,
                };

                if !should_supersede(analysis, &existing_mock, supersede_older) {
                    return Ok(false);
                }

                if existing_name != analysis.engine_name {
                    self.conn.execute(
                        "DELETE FROM position_analysis WHERE pos_hash = ?1 AND engine_family = ?2 AND multipv = ?3 AND engine_name = ?4",
                        params![hash_i64, family_str, analysis.multipv, existing_name],
                    )?;
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(err) => return Err(err.into()),
        }

        let pv_str = analysis.pv.join(" ");
        self.conn.execute(
            "INSERT OR REPLACE INTO position_analysis (
                pos_hash, fen, engine_family, engine_name, multipv, depth, seldepth,
                score_cp, score_mate, nodes, time_ms, nps, pv, quality_score, analyzed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                hash_i64,
                analysis.fen,
                family_str,
                analysis.engine_name,
                analysis.multipv,
                analysis.depth,
                analysis.seldepth,
                analysis.score_cp,
                analysis.score_mate,
                analysis.nodes.map(|n| n as i64),
                analysis.time_ms.map(|t| t as i64),
                analysis.nps.map(|n| n as i64),
                pv_str,
                analysis.quality_score,
                analysis.analyzed_at as i64,
            ],
        )?;

        Ok(true)
    }

    pub fn count_positions(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT pos_hash) FROM position_analysis",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute("DELETE FROM position_analysis", [])?;
        Ok(())
    }
}

pub struct AnalysisDebouncer {
    last_flush: Instant,
    last_persisted_depth: u32,
    buffered: BTreeMap<usize, CachedAnalysis>,
    current_pos_hash: Option<u64>,
}

impl Default for AnalysisDebouncer {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisDebouncer {
    pub const FAST_DEPTH_THRESHOLD: u32 = 20;
    pub const THROTTLE_DURATION: Duration = Duration::from_secs(10);

    pub fn new() -> Self {
        Self {
            last_flush: Instant::now(),
            last_persisted_depth: 0,
            buffered: BTreeMap::new(),
            current_pos_hash: None,
        }
    }

    pub fn push(&mut self, analysis: CachedAnalysis, now: Instant) -> Option<Vec<CachedAnalysis>> {
        let mut previous_flushed = None;
        if let Some(current) = self.current_pos_hash
            && current != analysis.pos_hash
        {
            // Position changed: return previous buffered analyses
            if !self.buffered.is_empty() {
                previous_flushed = Some(self.buffered.values().cloned().collect());
            }
            self.buffered.clear();
            self.last_flush = now;
            self.last_persisted_depth = 0;
        }
        self.current_pos_hash = Some(analysis.pos_hash);
        let depth = analysis.depth;
        self.buffered.insert(analysis.multipv, analysis);

        if previous_flushed.is_some() {
            return previous_flushed;
        }

        if depth < Self::FAST_DEPTH_THRESHOLD {
            if now.duration_since(self.last_flush) >= Self::THROTTLE_DURATION {
                self.last_flush = now;
                self.last_persisted_depth = depth;
                Some(self.buffered.values().cloned().collect())
            } else {
                None
            }
        } else {
            // Deep plies (>= 20) flush immediately on depth increment
            if depth > self.last_persisted_depth {
                self.last_flush = now;
                self.last_persisted_depth = depth;
                Some(self.buffered.values().cloned().collect())
            } else {
                None
            }
        }
    }

    pub fn flush(&mut self) -> Option<Vec<CachedAnalysis>> {
        if self.buffered.is_empty() {
            None
        } else {
            Some(self.buffered.values().cloned().collect())
        }
    }
}

pub enum CacheCommand {
    Store(CachedAnalysis),
    StoreBatch(Vec<CachedAnalysis>),
    Flush,
    Close,
}

#[derive(Clone)]
pub struct AnalysisCacheHandle {
    sender: Sender<CacheCommand>,
}

impl AnalysisCacheHandle {
    pub fn spawn(db_path: Option<PathBuf>, heuristic_supersede: bool) -> Self {
        let (sender, receiver) = mpsc::channel();
        let path = db_path.unwrap_or_else(AnalysisDb::default_path);

        thread::Builder::new()
            .name("brett-analysis-cache".to_string())
            .spawn(move || {
                let mut db = match AnalysisDb::open_at(&path) {
                    Ok(db) => db,
                    Err(err) => {
                        eprintln!(
                            "Failed to open analysis database at {}: {err}",
                            path.display()
                        );
                        return;
                    }
                };

                while let Ok(cmd) = receiver.recv() {
                    match cmd {
                        CacheCommand::Store(analysis) => {
                            let _ = db.store(&analysis, heuristic_supersede);
                        }
                        CacheCommand::StoreBatch(analyses) => {
                            for analysis in analyses {
                                let _ = db.store(&analysis, heuristic_supersede);
                            }
                        }
                        CacheCommand::Flush => {}
                        CacheCommand::Close => break,
                    }
                }
            })
            .expect("failed to spawn analysis cache worker thread");

        Self { sender }
    }

    pub fn store(&self, analysis: CachedAnalysis) {
        let _ = self.sender.send(CacheCommand::Store(analysis));
    }

    pub fn store_batch(&self, analyses: Vec<CachedAnalysis>) {
        if !analyses.is_empty() {
            let _ = self.sender.send(CacheCommand::StoreBatch(analyses));
        }
    }

    pub fn flush(&self) {
        let _ = self.sender.send(CacheCommand::Flush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "brett-test-{name}-{}-{nonce}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn sample_analysis(
        pos_hash: u64,
        family: EngineFamily,
        name: &str,
        multipv: usize,
        depth: u32,
        cp: i32,
        pv: &[&str],
    ) -> CachedAnalysis {
        CachedAnalysis {
            pos_hash,
            fen: "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string(),
            engine_family: family,
            engine_name: name.to_string(),
            multipv,
            depth,
            seldepth: Some(depth + 4),
            score_cp: Some(cp),
            score_mate: None,
            nodes: Some(1_000_000),
            time_ms: Some(500),
            nps: Some(2_000_000),
            pv: pv.iter().map(|s| s.to_string()).collect(),
            quality_score: depth as f64 * 35.0,
            analyzed_at: 1700000000,
        }
    }

    #[test]
    fn schema_creation_and_round_trip() {
        let db_path = test_db_path("round_trip");
        let mut db = AnalysisDb::open_at(&db_path).unwrap();

        assert_eq!(db.count_positions().unwrap(), 0);

        let entry = sample_analysis(
            0x12345678_abcdef01,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            25,
            32,
            &["e2e4", "e7e5"],
        );
        let stored = db.store(&entry, true).unwrap();
        assert!(stored);

        let retrieved = db.get_position(0x12345678_abcdef01).unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].engine_name, "Stockfish 17");
        assert_eq!(retrieved[0].depth, 25);
        assert_eq!(retrieved[0].pv, vec!["e2e4", "e7e5"]);
        assert_eq!(retrieved[0].score_cp, Some(32));

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn stockfish_and_leela_coexist_for_same_position() {
        let db_path = test_db_path("coexist");
        let mut db = AnalysisDb::open_at(&db_path).unwrap();

        let sf_entry = sample_analysis(
            0xdeadbeef_cafebabe,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            35,
            28,
            &["d2d4", "d7d5"],
        );
        let leela_entry = sample_analysis(
            0xdeadbeef_cafebabe,
            EngineFamily::Leela,
            "Lc0 v0.31.2",
            1,
            20,
            15,
            &["c2c4", "e7e6"],
        );

        db.store(&sf_entry, true).unwrap();
        db.store(&leela_entry, true).unwrap();

        let retrieved = db.get_position(0xdeadbeef_cafebabe).unwrap();
        assert_eq!(
            retrieved.len(),
            2,
            "both Stockfish and Leela must coexist for the position"
        );
        assert!(
            retrieved
                .iter()
                .any(|r| r.engine_family == EngineFamily::Stockfish)
        );
        assert!(
            retrieved
                .iter()
                .any(|r| r.engine_family == EngineFamily::Leela)
        );

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn multi_pv_storage_and_retrieval() {
        let db_path = test_db_path("multipv");
        let mut db = AnalysisDb::open_at(&db_path).unwrap();

        let pv1 = sample_analysis(
            0x11112222_33334444,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            22,
            40,
            &["e2e4"],
        );
        let pv2 = sample_analysis(
            0x11112222_33334444,
            EngineFamily::Stockfish,
            "Stockfish 17",
            2,
            22,
            25,
            &["d2d4"],
        );

        db.store(&pv1, true).unwrap();
        db.store(&pv2, true).unwrap();

        let retrieved = db.get_position(0x11112222_33334444).unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].multipv, 1);
        assert_eq!(retrieved[1].multipv, 2);

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn parse_engine_identities() {
        let sf17 = parse_engine_identity("Stockfish 17");
        assert_eq!(sf17.family, EngineFamily::Stockfish);
        assert_eq!(sf17.major, Some(17));
        assert_eq!(sf17.baseline_elo, 3830.0);

        let sf16_1 = parse_engine_identity("Stockfish 16.1");
        assert_eq!(sf16_1.family, EngineFamily::Stockfish);
        assert_eq!(sf16_1.major, Some(16));
        assert_eq!(sf16_1.minor, Some(1));
        assert_eq!(sf16_1.baseline_elo, 3790.0);

        let sf_dev = parse_engine_identity("Stockfish dev-20241018");
        assert_eq!(sf_dev.family, EngineFamily::Stockfish);
        assert_eq!(sf_dev.dev_date, Some(20241018));
        assert!(sf_dev.baseline_elo >= 3830.0);

        let leela = parse_engine_identity("Lc0 v0.31.2");
        assert_eq!(leela.family, EngineFamily::Leela);

        let generic = parse_engine_identity("Komodo 14");
        assert_eq!(generic.family, EngineFamily::Generic);
    }

    #[test]
    fn piecewise_search_strength() {
        assert_eq!(compute_search_strength(0), 0.0);
        assert_eq!(compute_search_strength(10), 10.0 * 50.0);
        assert_eq!(compute_search_strength(18), 18.0 * 50.0);
        assert_eq!(compute_search_strength(20), 18.0 * 50.0 + 2.0 * 35.0);
        assert_eq!(compute_search_strength(30), 18.0 * 50.0 + 12.0 * 35.0);
        assert_eq!(
            compute_search_strength(35),
            18.0 * 50.0 + 12.0 * 35.0 + 5.0 * 20.0
        );
    }

    #[test]
    fn quality_comparison_and_superseding() {
        let mut sf16_deep = sample_analysis(
            1,
            EngineFamily::Stockfish,
            "Stockfish 16",
            1,
            26,
            20,
            &["e2e4"],
        );
        sf16_deep.quality_score = compute_quality_rating(
            EngineFamily::Stockfish,
            3760.0,
            26,
            Some(30),
            Some(5_000_000),
            None,
        );

        let mut sf17_shallow = sample_analysis(
            1,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            20,
            20,
            &["e2e4"],
        );
        sf17_shallow.quality_score = compute_quality_rating(
            EngineFamily::Stockfish,
            3830.0,
            20,
            Some(24),
            Some(1_000_000),
            None,
        );

        // SF16 at depth 26 beats SF17 at depth 20:
        assert!(!should_supersede(&sf17_shallow, &sf16_deep, true));

        // But SF17 at depth 25 beats SF16 at depth 26:
        let mut sf17_deep = sample_analysis(
            1,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            25,
            20,
            &["e2e4"],
        );
        sf17_deep.quality_score = compute_quality_rating(
            EngineFamily::Stockfish,
            3830.0,
            25,
            Some(29),
            Some(3_000_000),
            None,
        );
        assert!(should_supersede(&sf17_deep, &sf16_deep, true));

        // When heuristic superseding is disabled, different engine names do NOT supersede:
        assert!(!should_supersede(&sf17_deep, &sf16_deep, false));

        // But identical engine name DOES supersede when depth >= existing depth:
        let sf16_shallower = sample_analysis(
            1,
            EngineFamily::Stockfish,
            "Stockfish 16",
            1,
            24,
            20,
            &["e2e4"],
        );
        assert!(should_supersede(&sf16_deep, &sf16_shallower, false));

        // Proven mate bonus dominates:
        let mate_analysis = CachedAnalysis {
            score_mate: Some(5),
            quality_score: compute_quality_rating(
                EngineFamily::Stockfish,
                3600.0,
                15,
                Some(15),
                Some(100_000),
                Some(5),
            ),
            ..sf16_shallower
        };
        assert!(should_supersede(&mate_analysis, &sf17_deep, true));
    }

    #[test]
    fn leela_centipawn_translation() {
        assert_eq!(leela_to_stockfish_cp(0), 0);
        let cp50 = leela_to_stockfish_cp(50);
        assert!((70..=90).contains(&cp50), "cp50: {cp50}");
        let cp100 = leela_to_stockfish_cp(100);
        assert!((150..=180).contains(&cp100), "cp100: {cp100}");
        let cp_neg100 = leela_to_stockfish_cp(-100);
        assert_eq!(cp_neg100, -cp100);
    }

    #[test]
    fn test_debouncer_throttling_and_deep_plies() {
        use std::time::{Duration, Instant};

        let mut debouncer = AnalysisDebouncer::new();
        let t0 = Instant::now();

        // 1. Rapid shallow plies (< 20) within 10s should buffer without flushing
        let a1 = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            10,
            20,
            &["e2e4"],
        );
        assert!(debouncer.push(a1, t0).is_none());

        let a2 = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            12,
            22,
            &["e2e4"],
        );
        assert!(debouncer.push(a2, t0 + Duration::from_secs(2)).is_none());

        // At 10s mark, shallow search flushes
        let a3 = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            15,
            25,
            &["e2e4"],
        );
        let flushed = debouncer.push(a3, t0 + Duration::from_secs(10));
        assert!(flushed.is_some());
        let batch = flushed.unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].depth, 15);

        // 2. Deep plies (>= 20) flush immediately on depth increment
        let a4 = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            20,
            28,
            &["e2e4"],
        );
        let flushed20 = debouncer.push(a4, t0 + Duration::from_secs(11));
        assert!(flushed20.is_some(), "depth 20 should flush immediately");

        let a5 = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            1,
            21,
            30,
            &["e2e4"],
        );
        let flushed21 = debouncer.push(a5, t0 + Duration::from_secs(12));
        assert!(flushed21.is_some(), "depth 21 should flush immediately");

        // 3. Search stop / BestMove flush
        let a6_buffered = sample_analysis(
            100,
            EngineFamily::Stockfish,
            "Stockfish 17",
            2,
            21,
            10,
            &["d2d4"],
        );
        // same depth so not flushed immediately:
        debouncer.push(a6_buffered, t0 + Duration::from_secs(13));
        let end_flush = debouncer.flush();
        assert!(end_flush.is_some());
        let final_batch = end_flush.unwrap();
        assert_eq!(
            final_batch.len(),
            2,
            "both multipv 1 and 2 should be returned on flush"
        );
    }
}
