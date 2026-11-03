//! Multi-segment reader for tiered LSM position indexing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use super::segment::IndexSegment;
use super::{EdgeRecord, MoveStat, PositionStats};

#[derive(Clone)]
pub struct SegmentedIndex {
    segments: Arc<RwLock<Vec<IndexSegment>>>,
    source: PathBuf,
}

impl SegmentedIndex {
    pub fn new(source: impl AsRef<Path>) -> Self {
        Self {
            segments: Arc::new(RwLock::new(Vec::new())),
            source: source.as_ref().to_path_buf(),
        }
    }

    pub fn from_segments(source: impl AsRef<Path>, segments: Vec<IndexSegment>) -> Self {
        Self {
            segments: Arc::new(RwLock::new(segments)),
            source: source.as_ref().to_path_buf(),
        }
    }

    pub fn add_segment(&self, segment: IndexSegment) {
        self.segments.write().expect("segments lock poisoned").push(segment);
    }

    pub fn set_segments(&self, segments: Vec<IndexSegment>) {
        *self.segments.write().expect("segments lock poisoned") = segments;
    }

    pub fn segments_arc(&self) -> Arc<RwLock<Vec<IndexSegment>>> {
        Arc::clone(&self.segments)
    }

    pub fn segment_count(&self) -> usize {
        self.segments.read().expect("segments lock poisoned").len()
    }

    pub fn path(&self) -> &Path {
        &self.source
    }

    pub fn game_count(&self) -> usize {
        let segs = self.segments.read().expect("segments lock poisoned");
        segs.iter().map(|s| s.game_count()).sum()
    }

    pub fn edge_count(&self) -> usize {
        let segs = self.segments.read().expect("segments lock poisoned");
        segs.iter().map(|s| s.edge_count()).sum()
    }

    pub fn position_count(&self) -> usize {
        let segs = self.segments.read().expect("segments lock poisoned");
        if segs.len() == 1 {
            segs[0].position_count()
        } else {
            segs.iter().map(|s| s.position_count()).sum()
        }
    }

    pub fn file_len(&self) -> u64 {
        let segs = self.segments.read().expect("segments lock poisoned");
        segs.iter().map(|s| s.file_len()).sum()
    }

    pub fn empty(&self) -> bool {
        let segs = self.segments.read().expect("segments lock poisoned");
        segs.is_empty() || segs.iter().all(|s| s.edge_count() == 0)
    }

    pub fn moves(&self, key: u64) -> Vec<MoveStat> {
        let segs = self.segments.read().expect("segments lock poisoned");
        if segs.is_empty() {
            return Vec::new();
        }

        let mut merged: HashMap<u32, EdgeRecord> = HashMap::new();
        for segment in segs.iter() {
            for record in segment.matching_records(key) {
                match merged.get_mut(&record.mv) {
                    Some(existing) => existing.merge_from(&record),
                    None => {
                        merged.insert(record.mv, record);
                    }
                }
            }
        }

        if merged.is_empty() {
            return Vec::new();
        }

        let pos_anchor = merged.values().map(|r| r.anchor_quarter).max().unwrap_or(0);
        let mut stats: Vec<MoveStat> = merged
            .into_values()
            .map(|mut record| {
                if pos_anchor != 0 && record.anchor_quarter != 0 {
                    record.advance_anchor(pos_anchor);
                } else if pos_anchor != 0 {
                    record.anchor_quarter = pos_anchor;
                }
                MoveStat {
                    key: record.key,
                    mv: record.mv,
                    games: record.games,
                    white: record.white,
                    draw: record.draw,
                    black: record.black,
                    elo_sum: record.elo_sum,
                    elo_count: record.elo_count,
                    first_date: record.first_date,
                    last_date: record.last_date,
                    anchor_quarter: record.anchor_quarter,
                    older_dated: record.older_dated,
                    buckets: record.buckets,
                }
            })
            .collect();

        stats.sort_by(|a, b| b.games.cmp(&a.games).then_with(|| a.mv.cmp(&b.mv)));
        stats
    }

    pub fn stats(&self, key: u64) -> Option<PositionStats> {
        let moves = self.moves(key);
        if moves.is_empty() {
            return None;
        }
        let mut stats = PositionStats::default();
        for mv in moves {
            stats.games += mv.games;
            stats.white += mv.white;
            stats.draw += mv.draw;
            stats.black += mv.black;
            stats.elo_sum += mv.elo_sum;
            stats.elo_count += mv.elo_count;
            if mv.first_date != 0
                && (stats.first_date == 0 || mv.first_date < stats.first_date)
            {
                stats.first_date = mv.first_date;
            }
            if mv.last_date > stats.last_date {
                stats.last_date = mv.last_date;
            }
        }
        Some(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use crate::position::{IndexHeader, IndexSegment};

    #[test]
    fn multi_segment_merges_records_accurately() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("brett-test-segmented-{}-{}", std::process::id(), unique));
        std::fs::create_dir_all(&dir).unwrap();
        let seg1_path = dir.join("seg1.bpx");
        let seg2_path = dir.join("seg2.bpx");

        let pos_key: u64 = 0x1234_5678_9abc_def0;
        let mv_code: u32 = 42;

        let rec1 = EdgeRecord {
            key: pos_key,
            mv: mv_code,
            games: 10,
            white: 4,
            draw: 4,
            black: 2,
            elo_sum: 25000,
            elo_count: 10,
            first_date: 20200101,
            last_date: 20210101,
            ..EdgeRecord::default()
        };

        let rec2 = EdgeRecord {
            key: pos_key,
            mv: mv_code,
            games: 5,
            white: 3,
            draw: 1,
            black: 1,
            elo_sum: 12500,
            elo_count: 5,
            first_date: 20190101,
            last_date: 20220101,
            ..EdgeRecord::default()
        };

        let header1 = IndexHeader {
            edge_count: 1,
            game_count: 10,
            position_count: 1,
            ..IndexHeader::default()
        };
        let header2 = IndexHeader {
            edge_count: 1,
            game_count: 5,
            position_count: 1,
            ..IndexHeader::default()
        };

        IndexSegment::write_records(&seg1_path, header1, &[rec1]).unwrap();
        IndexSegment::write_records(&seg2_path, header2, &[rec2]).unwrap();

        let s1 = IndexSegment::open(&seg1_path).unwrap();
        let s2 = IndexSegment::open(&seg2_path).unwrap();

        let index = SegmentedIndex::from_segments(dir.join("base"), vec![s1, s2]);

        assert_eq!(index.segment_count(), 2);
        assert_eq!(index.game_count(), 15);
        assert_eq!(index.edge_count(), 2);

        let moves = index.moves(pos_key);
        assert_eq!(moves.len(), 1);
        let m = &moves[0];
        assert_eq!(m.games, 15);
        assert_eq!(m.white, 7);
        assert_eq!(m.draw, 5);
        assert_eq!(m.black, 3);
        assert_eq!(m.elo_sum, 37500);
        assert_eq!(m.elo_count, 15);
        assert_eq!(m.first_date, 20190101);
        assert_eq!(m.last_date, 20220101);

        let stats = index.stats(pos_key).unwrap();
        assert_eq!(stats.games, 15);
        assert_eq!(stats.white, 7);
        assert_eq!(stats.draw, 5);
        assert_eq!(stats.black, 3);
        assert_eq!(stats.elo_sum, 37500);
        assert_eq!(stats.elo_count, 15);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
