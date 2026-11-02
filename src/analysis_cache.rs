//! Local persistent analysis cache.
//!
//! Stores engine analysis (engine name, depth, score, multipv, PV moves)
//! in SQLite with WAL mode. Supports coexistence of engine families (Stockfish,
//! Leela, Generic) and fine-grained replacement heuristics.

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineFamily {
    Stockfish,
    Leela,
    Generic,
}

impl EngineFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stockfish => "stockfish",
            Self::Leela => "leela",
            Self::Generic => "generic",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "stockfish" => Self::Stockfish,
            "leela" | "lc0" => Self::Leela,
            _ => Self::Generic,
        }
    }
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

            let pv = pv_str
                .split_whitespace()
                .map(str::to_string)
                .collect();

            Ok(CachedAnalysis {
                pos_hash: pos_hash_val as u64,
                fen,
                engine_family: EngineFamily::from_str(&family_str),
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

        let existing = check_stmt.query_row(
            params![hash_i64, family_str, analysis.multipv],
            |row| {
                let name: String = row.get(0)?;
                let depth: u32 = row.get(1)?;
                let quality: f64 = row.get(2)?;
                Ok((name, depth, quality))
            },
        );

        match existing {
            Ok((existing_name, existing_depth, existing_quality)) => {
                let identical_engine = existing_name == analysis.engine_name;
                if identical_engine {
                    if analysis.depth < existing_depth {
                        return Ok(false); // Do not downgrade depth
                    }
                } else if supersede_older {
                    // Supersede if incoming quality is higher
                    if analysis.quality_score < existing_quality {
                        return Ok(false);
                    }
                    // Delete the older different build for this family/multipv slot
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("brett-test-{name}-{}-{nonce}.db", std::process::id()));
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

        let entry = sample_analysis(0x12345678_abcdef01, EngineFamily::Stockfish, "Stockfish 17", 1, 25, 32, &["e2e4", "e7e5"]);
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

        let sf_entry = sample_analysis(0xdeadbeef_cafebabe, EngineFamily::Stockfish, "Stockfish 17", 1, 35, 28, &["d2d4", "d7d5"]);
        let leela_entry = sample_analysis(0xdeadbeef_cafebabe, EngineFamily::Leela, "Lc0 v0.31.2", 1, 20, 15, &["c2c4", "e7e6"]);

        db.store(&sf_entry, true).unwrap();
        db.store(&leela_entry, true).unwrap();

        let retrieved = db.get_position(0xdeadbeef_cafebabe).unwrap();
        assert_eq!(retrieved.len(), 2, "both Stockfish and Leela must coexist for the position");
        assert!(retrieved.iter().any(|r| r.engine_family == EngineFamily::Stockfish));
        assert!(retrieved.iter().any(|r| r.engine_family == EngineFamily::Leela));

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn multi_pv_storage_and_retrieval() {
        let db_path = test_db_path("multipv");
        let mut db = AnalysisDb::open_at(&db_path).unwrap();

        let pv1 = sample_analysis(0x11112222_33334444, EngineFamily::Stockfish, "Stockfish 17", 1, 22, 40, &["e2e4"]);
        let pv2 = sample_analysis(0x11112222_33334444, EngineFamily::Stockfish, "Stockfish 17", 2, 22, 25, &["d2d4"]);

        db.store(&pv1, true).unwrap();
        db.store(&pv2, true).unwrap();

        let retrieved = db.get_position(0x11112222_33334444).unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].multipv, 1);
        assert_eq!(retrieved[1].multipv, 2);

        let _ = std::fs::remove_file(&db_path);
    }
}
