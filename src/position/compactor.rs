//! Asynchronous K-way segment compactor.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use super::segment::IndexSegment;
use super::segmented_index::SegmentedIndex;
use super::{BuildReport, MergeSource, write_merged_sources};
use crate::library::SourceMeta;

/// Compact all active segments in `target_index` into a single consolidated
/// `.brettpos` file at `consolidated_path` using a streaming K-way merge.
///
/// Once compaction completes, `target_index` is atomically updated to point
/// to the single consolidated file, and temporary segment files are removed.
pub fn compact_segments(
    target_index: &SegmentedIndex,
    consolidated_path: impl AsRef<Path>,
    meta: SourceMeta,
    max_plies: u32,
) -> Result<BuildReport> {
    let consolidated_path = consolidated_path.as_ref();
    let started = Instant::now();

    let segs_arc = target_index.segments_arc();
    let (report, old_paths) = {
        let guard = segs_arc.read().expect("segments lock poisoned");
        if guard.len() <= 1
            && guard
                .first()
                .map(|s| s.path() == consolidated_path)
                .unwrap_or(false)
        {
            return Ok(BuildReport {
                games: target_index.game_count(),
                edges: target_index.edge_count(),
                positions: target_index.position_count(),
                elapsed: started.elapsed(),
                bytes: target_index.file_len(),
            });
        }

        let total_games = guard.iter().map(|s| s.game_count()).sum();
        let sources: Vec<MergeSource<'_>> = guard
            .iter()
            .filter(|s| s.edge_count() > 0)
            .map(|s| MergeSource::Segment {
                segment: s,
                pos: 0,
                end: s.edge_count(),
            })
            .collect();

        let old_paths: Vec<PathBuf> = guard
            .iter()
            .map(|s| s.path().to_path_buf())
            .filter(|p| p != consolidated_path)
            .collect();

        let rep = write_merged_sources(
            consolidated_path,
            meta,
            max_plies,
            total_games,
            started,
            sources,
        )?;

        (rep, old_paths)
    };

    // Open the new consolidated segment and swap it into the reader atomically
    let consolidated_seg = IndexSegment::open(consolidated_path)?;
    target_index.set_segments(vec![consolidated_seg]);

    // Unlink old temporary segment files
    for path in old_paths {
        let _ = fs::remove_file(path);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::Position;
    use crate::position::{BuildOptions, EdgeRecord, IndexHeader};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn compactor_merges_multiple_segments_and_removes_temporaries() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "brett-test-compact-{}-{}",
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let seg1_path = dir.join("seg1.tmp.bpx");
        let seg2_path = dir.join("seg2.tmp.bpx");
        let consolidated_path = dir.join("base.brettpos");

        let pos1: u64 = 0x1111_2222_3333_4444;
        let pos2: u64 = 0x5555_6666_7777_8888;
        let mv_a: u32 = 10;
        let mv_b: u32 = 20;

        let rec1 = EdgeRecord {
            key: pos1,
            mv: mv_a,
            games: 10,
            white: 6,
            draw: 2,
            black: 2,
            elo_sum: 24000,
            elo_count: 10,
            ..EdgeRecord::default()
        };
        let rec2 = EdgeRecord {
            key: pos1,
            mv: mv_b,
            games: 4,
            white: 2,
            draw: 1,
            black: 1,
            elo_sum: 9600,
            elo_count: 4,
            ..EdgeRecord::default()
        };
        let header1 = IndexHeader {
            edge_count: 2,
            game_count: 14,
            position_count: 1,
            ..IndexHeader::default()
        };
        IndexSegment::write_records(&seg1_path, header1, &[rec1, rec2]).unwrap();

        let rec3 = EdgeRecord {
            key: pos1,
            mv: mv_a,
            games: 5,
            white: 3,
            draw: 1,
            black: 1,
            elo_sum: 12000,
            elo_count: 5,
            ..EdgeRecord::default()
        };
        let rec4 = EdgeRecord {
            key: pos2,
            mv: mv_a,
            games: 8,
            white: 4,
            draw: 2,
            black: 2,
            elo_sum: 19200,
            elo_count: 8,
            ..EdgeRecord::default()
        };
        let header2 = IndexHeader {
            edge_count: 2,
            game_count: 13,
            position_count: 2,
            ..IndexHeader::default()
        };
        IndexSegment::write_records(&seg2_path, header2, &[rec3, rec4]).unwrap();

        let s1 = IndexSegment::open(&seg1_path).unwrap();
        let s2 = IndexSegment::open(&seg2_path).unwrap();
        let index = SegmentedIndex::from_segments(&consolidated_path, vec![s1, s2]);
        assert_eq!(index.segment_count(), 2);

        let report =
            compact_segments(&index, &consolidated_path, SourceMeta::default(), u32::MAX).unwrap();
        assert_eq!(report.games, 27);
        assert_eq!(report.positions, 2);
        assert_eq!(report.edges, 3); // (pos1, mv_a), (pos1, mv_b), (pos2, mv_a)

        // Index now has 1 consolidated segment
        assert_eq!(index.segment_count(), 1);
        assert_eq!(index.game_count(), 27);

        // Check merged move stats for pos1, mv_a (10 + 5 = 15 games)
        let moves = index.moves(pos1);
        assert_eq!(moves.len(), 2);
        let m_a = moves.iter().find(|m| m.mv == mv_a).unwrap();
        assert_eq!(m_a.games, 15);
        assert_eq!(m_a.white, 9);
        assert_eq!(m_a.draw, 3);
        assert_eq!(m_a.black, 3);
        assert_eq!(m_a.elo_sum, 36000);

        // Old segment files are removed
        assert!(!seg1_path.exists());
        assert!(!seg2_path.exists());
        assert!(consolidated_path.exists());

        // Now test appending a delta segment
        let mut pgn_game = crate::domain::Game::default();
        pgn_game.set_header("Event", "Delta Game".to_string());
        pgn_game.set_header("Result", "1-0".to_string());
        // play e4 on root pos
        let start_pos = crate::chess::Chess::default();
        let e4_code = crate::position::encode_move(
            crate::chess::UciMove::from_ascii(b"e2e4")
                .unwrap()
                .to_move(&start_pos)
                .unwrap(),
        )
        .unwrap();
        let child_pos = start_pos
            .clone()
            .play(
                crate::chess::UciMove::from_ascii(b"e2e4")
                    .unwrap()
                    .to_move(&start_pos)
                    .unwrap(),
            )
            .unwrap();
        pgn_game.nodes.push(crate::domain::Node {
            mv: Some(crate::domain::Ply {
                san: "e4".to_string(),
                uci: "e2e4".to_string(),
                number: 1,
                color: crate::chess::Color::White,
            }),
            comment: String::new(),
            clock: None,
            eval: None,
            nags: Vec::new(),
            parent: Some(0),
            children: Vec::new(),
            pos: child_pos,
        });
        pgn_game.nodes[0].children.push(1);

        let delta_path = dir.join("delta-001.bpx");
        index
            .append_delta_segment(
                &[pgn_game],
                SourceMeta::default(),
                &delta_path,
                BuildOptions::default(),
            )
            .unwrap();

        assert_eq!(index.segment_count(), 2);
        assert_eq!(index.game_count(), 28);
        let root_moves = index.moves(start_pos.position_hash());
        assert_eq!(root_moves.len(), 1);
        assert_eq!(root_moves[0].mv, e4_code);
        assert_eq!(root_moves[0].games, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
