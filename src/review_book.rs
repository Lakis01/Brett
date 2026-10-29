//! Opening-book prefix detection for game review.
//!
//! Walks the mainline from the starting position and determines how many
//! leading plies are still in opening theory according to the user's open
//! native book, local opening-tree position index, or the public Lichess
//! Masters explorer.

use std::time::{Duration, Instant};

use crate::book::Book;
use crate::chess::{CastlingMode, Chess, EnPassantMode, Fen, Move, UciMove};
use crate::cloud::{self, Explorer};
use crate::domain::Game;
use crate::position::{self, PositionIndex};

/// Default total wall-clock budget for online Masters explorer queries during
/// a single review start.
pub const DEFAULT_MASTERS_BUDGET: Duration = Duration::from_secs(5);

/// A source of opening-book move membership for a position.
pub trait BookSource {
    fn is_book_move(&mut self, pos: &Chess, mv: Option<Move>, played_uci: &str) -> bool;
}

/// Native opening book (`.bin`, `.ctg`, `.abk`, `.bkt`).
pub struct NativeBookSource<'a> {
    book: &'a Book,
}

impl<'a> NativeBookSource<'a> {
    pub fn new(book: &'a Book) -> Self {
        Self { book }
    }
}

impl BookSource for NativeBookSource<'_> {
    fn is_book_move(&mut self, pos: &Chess, mv: Option<Move>, played_uci: &str) -> bool {
        let Ok(moves) = self.book.moves(pos) else {
            return false;
        };
        moves.iter().any(|entry| {
            mv.is_some_and(|m| entry.mv == m)
                || UciMove::from_move(entry.mv, CastlingMode::Standard).to_string() == played_uci
        })
    }
}

/// Local memory-mapped opening-tree [`PositionIndex`].
pub struct PositionIndexSource<'a> {
    index: &'a PositionIndex,
    min_games: u32,
}

impl<'a> PositionIndexSource<'a> {
    pub fn new(index: &'a PositionIndex, min_games: u32) -> Self {
        Self {
            index,
            min_games: min_games.max(1),
        }
    }
}

impl BookSource for PositionIndexSource<'_> {
    fn is_book_move(&mut self, pos: &Chess, mv: Option<Move>, played_uci: &str) -> bool {
        let key = position::position_key(pos);
        let target_code = mv.and_then(position::encode_move);
        self.index.moves(key).into_iter().any(|stat| {
            if stat.games < self.min_games {
                return false;
            }
            if let Some(code) = target_code
                && stat.mv == code
            {
                return true;
            }
            stat.to_move(pos).is_some_and(|decoded| {
                UciMove::from_move(decoded, CastlingMode::Standard).to_string() == played_uci
            })
        })
    }
}

/// Online Lichess Masters opening explorer source with a shared time budget.
pub struct MastersExplorerSource {
    min_games: u64,
    lichess_token: Option<String>,
    deadline: Instant,
    disabled: bool,
}

impl MastersExplorerSource {
    pub fn new(min_games: u32, lichess_token: Option<String>, budget: Duration) -> Self {
        Self {
            min_games: u64::from(min_games.max(1)),
            lichess_token,
            deadline: Instant::now() + budget,
            disabled: false,
        }
    }
}

impl BookSource for MastersExplorerSource {
    fn is_book_move(&mut self, pos: &Chess, mv: Option<Move>, played_uci: &str) -> bool {
        if self.disabled {
            return false;
        }
        let now = Instant::now();
        let Some(remaining) = self.deadline.checked_duration_since(now) else {
            self.disabled = true;
            return false;
        };
        if remaining < Duration::from_millis(100) {
            self.disabled = true;
            return false;
        }
        let fen = Fen::from_position(pos, EnPassantMode::Legal).to_string();
        let per_request = remaining.min(Duration::from_millis(1_500));
        match cloud::fetch_masters_explorer_with_timeout(
            &fen,
            self.lichess_token.as_deref(),
            per_request,
        ) {
            Ok(explorer) => explorer_has_move(&explorer, pos, mv, played_uci, self.min_games),
            Err(_) => {
                self.disabled = true;
                false
            }
        }
    }
}

/// Check whether a parsed [`Explorer`] response contains `mv` / `played_uci`
/// with at least `min_games` games.
pub fn explorer_has_move(
    explorer: &Explorer,
    pos: &Chess,
    mv: Option<Move>,
    played_uci: &str,
    min_games: u64,
) -> bool {
    explorer.moves.iter().any(|entry| {
        if entry.total() < min_games {
            return false;
        }
        if entry.uci == played_uci {
            return true;
        }
        if let Some(mv) = mv
            && let Ok(uci) = UciMove::from_ascii(entry.uci.as_bytes())
            && let Ok(parsed) = uci.to_move(pos)
        {
            return parsed == mv;
        }
        false
    })
}

/// Return the number of leading mainline plies in `game` that are covered by
/// at least one of `sources`, capped at `max_plies`.
///
/// Detection stops at the first non-book move.
pub fn detect_prefix(game: &Game, sources: &mut [&mut dyn BookSource], max_plies: usize) -> usize {
    if sources.is_empty() || max_plies == 0 {
        return 0;
    }
    let mut plies = 0usize;
    let mut cur = 0usize;
    while plies < max_plies {
        let Some(&next) = game.nodes[cur].children.first() else {
            break;
        };
        let Some(ply) = game.nodes[next].mv.as_ref() else {
            break;
        };
        if ply.uci == "0000" {
            break;
        }
        let pos = &game.nodes[cur].pos;
        let mv = UciMove::from_ascii(ply.uci.as_bytes())
            .ok()
            .and_then(|uci| uci.to_move(pos).ok());
        let in_book = sources
            .iter_mut()
            .any(|source| source.is_book_move(pos, mv, &ply.uci));
        if !in_book {
            break;
        }
        plies += 1;
        cur = next;
    }
    plies
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::domain::parse_pgn;
    use crate::position::BuildOptions;

    struct FakeBook {
        allowed_ucis: HashSet<String>,
    }

    impl FakeBook {
        fn new(ucis: &[&str]) -> Self {
            Self {
                allowed_ucis: ucis.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl BookSource for FakeBook {
        fn is_book_move(&mut self, _pos: &Chess, _mv: Option<Move>, played_uci: &str) -> bool {
            self.allowed_ucis.contains(played_uci)
        }
    }

    fn sample_game() -> Game {
        parse_pgn(b"1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 *\n")
            .unwrap()
            .remove(0)
    }

    #[test]
    fn detects_prefix_and_stops_at_first_non_book_move() {
        let game = sample_game();
        // Allow 1. e4 e5 2. Nf3, skip 2... Nc6, even though 3. Bb5 is in the set.
        let mut fake = FakeBook::new(&["e2e4", "e7e5", "g1f3", "f1b5"]);
        let mut sources: [&mut dyn BookSource; 1] = [&mut fake];
        assert_eq!(detect_prefix(&game, &mut sources, 30), 3);
    }

    #[test]
    fn respects_max_plies_cap() {
        let game = sample_game();
        let mut fake = FakeBook::new(&["e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6"]);
        let mut sources: [&mut dyn BookSource; 1] = [&mut fake];
        assert_eq!(detect_prefix(&game, &mut sources, 4), 4);
    }

    #[test]
    fn combines_sources_in_priority_order() {
        let game = sample_game();
        let mut primary = FakeBook::new(&["e2e4", "e7e5"]);
        let mut fallback = FakeBook::new(&["g1f3", "b8c6"]);
        let mut sources: [&mut dyn BookSource; 2] = [&mut primary, &mut fallback];
        assert_eq!(detect_prefix(&game, &mut sources, 30), 4);
    }

    #[test]
    fn position_index_source_enforces_min_games_threshold() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("brett-review-book-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pgn_path = dir.join("corpus.pgn");
        let idx_path = dir.join("corpus.bpos");
        std::fs::write(
            &pgn_path,
            b"[Event \"1\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n\n[Event \"2\"]\n\n1. e4 e5 2. Nf3 Nf6 *\n\n[Event \"3\"]\n\n1. e4 e5 2. Nf3 d6 *\n",
        )
        .unwrap();
        PositionIndex::build_to(&pgn_path, &idx_path, BuildOptions::default(), |_| {}).unwrap();
        let index = PositionIndex::open(&idx_path).unwrap();

        let game = parse_pgn(b"1. e4 e5 2. Nf3 Nc6 *\n").unwrap().remove(0);
        // 1. e4, 1... e5, 2. Nf3 appear in 3 games; 2... Nc6 appears in only 1 game.
        let mut src_min_3 = PositionIndexSource::new(&index, 3);
        let mut sources: [&mut dyn BookSource; 1] = [&mut src_min_3];
        assert_eq!(detect_prefix(&game, &mut sources, 30), 3);

        let mut src_min_1 = PositionIndexSource::new(&index, 1);
        let mut sources: [&mut dyn BookSource; 1] = [&mut src_min_1];
        assert_eq!(detect_prefix(&game, &mut sources, 30), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn masters_explorer_helper_checks_move_total_and_castling_uci() {
        let json = r#"{
            "white": 100,
            "draws": 200,
            "black": 50,
            "moves": [
                {"uci": "e2e4", "san": "e4", "white": 50, "draws": 100, "black": 25, "averageRating": 2650},
                {"uci": "g2g4", "san": "g4", "white": 1, "draws": 0, "black": 1, "averageRating": 2400}
            ],
            "topGames": []
        }"#;
        let explorer = crate::cloud::parse_explorer(json).unwrap();
        let pos = Chess::default();
        let e4 = UciMove::from_ascii(b"e2e4").unwrap().to_move(&pos).unwrap();
        let g4 = UciMove::from_ascii(b"g2g4").unwrap().to_move(&pos).unwrap();

        assert!(explorer_has_move(&explorer, &pos, Some(e4), "e2e4", 3));
        assert!(!explorer_has_move(&explorer, &pos, Some(g4), "g2g4", 3));
        assert!(explorer_has_move(&explorer, &pos, Some(g4), "g2g4", 2));
    }
}
