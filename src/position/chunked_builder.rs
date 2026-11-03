//! Parallel chunked position index builder with in-memory edge aggregation.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;

use super::segment::IndexSegment;
use super::segmented_index::SegmentedIndex;
use super::{BuildOptions, BuildProgress, BuildReport, EdgeRecord, GameMeta, IndexHeader};
use crate::library::GameLibrary;

pub const DEFAULT_CHUNK_SIZE: usize = 250_000;

#[derive(Clone, Copy, Debug)]
pub struct ChunkedBuildOptions {
    pub build_options: BuildOptions,
    pub chunk_size: usize,
}

impl Default for ChunkedBuildOptions {
    fn default() -> Self {
        Self {
            build_options: BuildOptions::default(),
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

/// Build a position index across multiple chunks in parallel using Rayon.
///
/// Each chunk aggregates edges in memory, sorts them, and flushes an immutable
/// `.tmp_segment_XXX.bpx` file. If `live_index` is provided, flushed segments
/// are registered progressively so that opening-tree queries work immediately.
struct ChunkResult {
    games: usize,
    edges: usize,
    unique_positions: usize,
}

pub fn build_chunked_parallel<F>(
    library: &dyn GameLibrary,
    cache_base_path: impl AsRef<Path>,
    options: ChunkedBuildOptions,
    live_index: Option<&SegmentedIndex>,
    progress: F,
) -> Result<BuildReport>
where
    F: Fn(BuildProgress) + Send + Sync,
{
    let started = Instant::now();
    let cache_base = cache_base_path.as_ref();
    let meta = library.source_meta()?;
    let total_games = library.len();

    if total_games == 0 {
        let header = IndexHeader {
            source_len: meta.len,
            source_mtime_secs: meta.mtime_secs,
            source_mtime_nanos: meta.mtime_nanos,
            max_plies: options.build_options.max_plies,
            edge_count: 0,
            game_count: 0,
            position_count: 0,
            build_millis: started.elapsed().as_millis() as u64,
        };
        IndexSegment::write_records(cache_base, header, &[])?;
        if let Some(target) = live_index {
            target.set_segments(vec![IndexSegment::open(cache_base)?]);
        }
        return Ok(BuildReport::default());
    }

    let chunk_size = options.chunk_size.max(1);
    let chunk_ranges: Vec<(usize, usize, usize)> = (0..total_games)
        .step_by(chunk_size)
        .enumerate()
        .map(|(chunk_id, start)| {
            let end = (start + chunk_size).min(total_games);
            (chunk_id, start, end)
        })
        .collect();

    let completed_games = Arc::new(AtomicUsize::new(0));
    let completed_edges = Arc::new(AtomicUsize::new(0));

    let segment_results: Result<Vec<ChunkResult>> = chunk_ranges
        .into_par_iter()
        .map(|(chunk_id, start, end)| {
            let mut records: HashMap<(u64, u32), EdgeRecord> = HashMap::new();
            let mut chunk_games = 0usize;

            for game_idx in start..end {
                let indexed_game = &library.games()[game_idx];
                let game_meta = GameMeta::from_shared_tags(indexed_game.headers());
                if options
                    .build_options
                    .rating_filter
                    .is_some_and(|filter| !filter.accepts(&game_meta))
                {
                    continue;
                }

                let mut game_keys = HashSet::new();
                let streamed = library.stream_mainline(
                    game_idx,
                    options.build_options.max_plies,
                    &mut |key, mv| {
                        if !game_keys.insert(key) {
                            return;
                        }
                        let record = records.entry((key, mv)).or_default();
                        record.key = key;
                        record.mv = mv;
                        game_meta.add_to(record);
                    },
                )?;

                if !streamed {
                    let game = library.load(game_idx)?;
                    let mut node = 0usize;
                    let mut plies = 0u32;
                    while let Some(&child) = game.nodes[node].children.first() {
                        let parent = &game.nodes[node];
                        let Some(ply) = game.nodes[child].mv.as_ref() else {
                            break;
                        };
                        if ply.uci == "0000" {
                            break;
                        }
                        let Ok(ucimove) = crate::chess::UciMove::from_ascii(ply.uci.as_bytes())
                        else {
                            break;
                        };
                        let Ok(mv) = ucimove.to_move(&parent.pos) else {
                            break;
                        };
                        let Some(code) = crate::position::encode_move(mv) else {
                            break;
                        };
                        let key = parent.pos.position_hash();
                        if !game_keys.insert(key) {
                            break;
                        }
                        let record = records.entry((key, code)).or_default();
                        record.key = key;
                        record.mv = code;
                        game_meta.add_to(record);
                        node = child;
                        plies += 1;
                        if plies >= options.build_options.max_plies {
                            break;
                        }
                    }
                }

                chunk_games += 1;
            }

            // Sort edges by (key, mv)
            let mut sorted_records: Vec<EdgeRecord> = records.into_values().collect();
            sorted_records.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));

            let mut unique_positions = 0usize;
            let mut last_key = None;
            for r in &sorted_records {
                if last_key != Some(r.key) {
                    unique_positions += 1;
                    last_key = Some(r.key);
                }
            }

            let seg_path = cache_base.with_extension(format!("tmp_segment_{chunk_id:04}.bpx"));

            let seg_header = IndexHeader {
                source_len: meta.len,
                source_mtime_secs: meta.mtime_secs,
                source_mtime_nanos: meta.mtime_nanos,
                max_plies: options.build_options.max_plies,
                edge_count: sorted_records.len() as u64,
                game_count: chunk_games as u64,
                position_count: unique_positions as u64,
                build_millis: started.elapsed().as_millis() as u64,
            };

            IndexSegment::write_records(&seg_path, seg_header, &sorted_records)?;

            let seg = IndexSegment::open(&seg_path)?;
            if let Some(target) = live_index {
                target.add_segment(seg);
            }

            let games_done =
                completed_games.fetch_add(chunk_games, Ordering::Relaxed) + chunk_games;
            let edges_done = completed_edges.fetch_add(sorted_records.len(), Ordering::Relaxed)
                + sorted_records.len();

            progress(BuildProgress {
                games: games_done,
                edges: edges_done,
                elapsed: started.elapsed(),
            });

            Ok(ChunkResult {
                games: chunk_games,
                edges: sorted_records.len(),
                unique_positions,
            })
        })
        .collect();

    let segments = segment_results?;
    let total_indexed_games: usize = segments.iter().map(|s| s.games).sum();
    let total_indexed_edges: usize = segments.iter().map(|s| s.edges).sum();
    let total_positions: usize = segments.iter().map(|s| s.unique_positions).sum();

    Ok(BuildReport {
        games: total_indexed_games,
        edges: total_indexed_edges,
        positions: total_positions,
        elapsed: started.elapsed(),
        bytes: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::PgnLibrary;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SAMPLE_PGN: &str = r#"[Event "Game 1"]
[White "Player A"]
[Black "Player B"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0

[Event "Game 2"]
[White "Player C"]
[Black "Player D"]
[Result "0-1"]

1. e4 c5 2. Nf3 d6 3. d4 cxd4 0-1

[Event "Game 3"]
[White "Player E"]
[Black "Player F"]
[Result "1/2-1/2"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1/2-1/2

[Event "Game 4"]
[White "Player G"]
[Black "Player H"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nf6 3. Nxe5 d6 1-0
"#;

    #[test]
    fn chunked_builder_processes_chunks_in_parallel() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "brett-test-chunked-{}-{}",
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let pgn_path = dir.join("test.pgn");
        std::fs::write(&pgn_path, SAMPLE_PGN).unwrap();

        let library = PgnLibrary::build(&pgn_path).unwrap();
        assert_eq!(library.len(), 4);

        let live_index = SegmentedIndex::new(dir.join("live"));
        let cache_base = dir.join("base.brettpos");

        let options = ChunkedBuildOptions {
            build_options: BuildOptions::default(),
            chunk_size: 2, // 2 games per chunk -> 2 chunks
        };

        let report =
            build_chunked_parallel(&library, &cache_base, options, Some(&live_index), |_| {})
                .unwrap();

        assert_eq!(report.games, 4);
        assert_eq!(live_index.segment_count(), 2);
        assert_eq!(live_index.game_count(), 4);

        // Verify e4 was played in games 1, 2, 4 (3 games)
        let root_key = crate::chess::Chess::default().position_hash();
        let moves = live_index.moves(root_key);
        let e4_stat = moves
            .iter()
            .find(|m| m.san(&crate::chess::Chess::default()).as_deref() == Some("e4"))
            .unwrap();
        assert_eq!(e4_stat.games, 3);
        assert_eq!(e4_stat.white, 2);
        assert_eq!(e4_stat.black, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
