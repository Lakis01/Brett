//! Native ECO opening classification using the CC0 `lichess-org/chess-openings`
//! dataset (`assets/eco/a.tsv`…`e.tsv`).
//!
//! Each dataset row defines an ECO code, an opening name, and its canonical PGN
//! move sequence from the standard starting position. [`EcoTable`] replays
//! those lines once into a map keyed by Brett's position hash. When several
//! dataset entries transpose to the same position, the table deterministically
//! keeps the entry with the shortest defining line, breaking ties by
//! lexicographically smallest ECO code and opening name.
//!
//! Classifying a game walks its mainline from the standard start position,
//! stopping cleanly at the first null or illegal move, and returns the deepest
//! position that has a named entry (`O(plies)` per game, independent of the
//! position index).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::chess::{CastlingMode, Chess, Position, SanPlus, UciMove};
use crate::domain::Game;
use crate::position;

const TSV_FILES: [&str; 5] = [
    include_str!("../assets/eco/a.tsv"),
    include_str!("../assets/eco/b.tsv"),
    include_str!("../assets/eco/c.tsv"),
    include_str!("../assets/eco/d.tsv"),
    include_str!("../assets/eco/e.tsv"),
];

/// Deepest ply examined when classifying a game's mainline. The longest line
/// in `lichess-org/chess-openings` is 36 plies; 64 gives generous headroom for
/// transpositions while keeping classification bounded on long games.
const MAX_CLASSIFY_PLIES: usize = 64;

/// One named opening entry from the ECO dataset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcoEntry {
    pub eco: String,
    pub name: String,
    pub plies: u16,
}

/// Position-indexed ECO opening table.
pub struct EcoTable {
    by_position: HashMap<u64, EcoEntry>,
}

/// Outcome of applying ECO classification to a single game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassifyOutcome {
    /// `ECO` and `Opening` tags were written (or completed).
    Updated,
    /// Existing `ECO`/`Opening` tags were preserved because `overwrite` was false.
    PreservedExisting,
    /// The game did not match any named opening (custom start, Chess960, empty,
    /// or left the opening book before any named position).
    Unclassified,
}

/// Summary of classifying a batch of games.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassifySummary {
    pub total: usize,
    pub updated: usize,
    pub preserved: usize,
    pub unclassified: usize,
}

impl ClassifySummary {
    pub fn record(&mut self, outcome: ClassifyOutcome) {
        self.total += 1;
        match outcome {
            ClassifyOutcome::Updated => self.updated += 1,
            ClassifyOutcome::PreservedExisting => self.preserved += 1,
            ClassifyOutcome::Unclassified => self.unclassified += 1,
        }
    }
}

static GLOBAL_TABLE: OnceLock<EcoTable> = OnceLock::new();

/// Return the shared, lazily initialized ECO table built from the embedded CC0
/// `lichess-org/chess-openings` TSV files.
pub fn global_table() -> &'static EcoTable {
    GLOBAL_TABLE.get_or_init(EcoTable::from_embedded)
}

/// Classify a game's mainline against the shared ECO table without modifying
/// its headers.
pub fn classify_game(game: &Game) -> Option<&'static EcoEntry> {
    global_table().classify_game(game)
}

/// Classify a game and write its `ECO` and `Opening` tags according to
/// `overwrite`.
///
/// When `overwrite` is `false`, existing non-placeholder `ECO` and `Opening`
/// tags are preserved so imported metadata is never silently rewritten. If a
/// game has a matching `ECO` tag but no `Opening` tag, the missing `Opening`
/// tag is filled in.
pub fn apply_classification(game: &mut Game, overwrite: bool) -> ClassifyOutcome {
    global_table().apply_to_game(game, overwrite)
}

impl EcoTable {
    /// Build the table from the embedded `a.tsv`…`e.tsv` files.
    pub fn from_embedded() -> Self {
        Self::from_tsv_sources(&TSV_FILES)
    }

    /// Build a table from one or more TSV strings (`eco\tname\tpgn`).
    pub fn from_tsv_sources(sources: &[&str]) -> Self {
        let mut by_position = HashMap::with_capacity(4096);
        for source in sources {
            for (line_idx, raw_line) in source.lines().enumerate() {
                let line = raw_line.trim();
                if line.is_empty() {
                    continue;
                }
                if line_idx == 0 && line.starts_with("eco\t") {
                    continue;
                }
                let mut fields = line.splitn(3, '\t');
                let (Some(eco), Some(name), Some(pgn)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                let Some((key, plies)) = replay_defining_pgn(pgn) else {
                    continue;
                };
                let candidate = EcoEntry {
                    eco: eco.trim().to_string(),
                    name: name.trim().to_string(),
                    plies,
                };
                match by_position.get_mut(&key) {
                    Some(existing) => {
                        if prefers_entry(&candidate, existing) {
                            *existing = candidate;
                        }
                    }
                    None => {
                        by_position.insert(key, candidate);
                    }
                }
            }
        }
        Self { by_position }
    }

    /// Number of distinct positions in the table.
    pub fn len(&self) -> usize {
        self.by_position.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_position.is_empty()
    }

    /// Look up a single position by its hash key.
    pub fn get(&self, key: u64) -> Option<&EcoEntry> {
        self.by_position.get(&key)
    }

    /// Replay a game's mainline and return the deepest matching opening entry.
    pub fn classify_game<'a>(&'a self, game: &Game) -> Option<&'a EcoEntry> {
        let root = game.nodes.first()?;
        if !is_standard_start(game, &root.pos) {
            return None;
        }

        let mut pos = Chess::default();
        let mut node = 0usize;
        let mut plies = 0usize;
        let mut deepest = None;

        while let Some(&child) = game.nodes[node].children.first() {
            if plies >= MAX_CLASSIFY_PLIES {
                break;
            }
            let Some(ply) = game.nodes[child].mv.as_ref() else {
                break;
            };
            if ply.uci == "0000" {
                break;
            }
            let Ok(uci) = UciMove::from_ascii(ply.uci.as_bytes()) else {
                break;
            };
            let Ok(mv) = uci.to_move(&pos) else {
                break;
            };
            let Ok(after) = pos.play(mv) else {
                break;
            };
            pos = after;
            plies += 1;
            let key = position::position_key(&pos);
            if let Some(entry) = self.by_position.get(&key) {
                deepest = Some(entry);
            }
            node = child;
        }

        deepest
    }

    /// Apply classification to `game`, respecting `overwrite`.
    pub fn apply_to_game(&self, game: &mut Game, overwrite: bool) -> ClassifyOutcome {
        let Some(entry) = self.classify_game(game) else {
            return ClassifyOutcome::Unclassified;
        };

        let existing_eco = meaningful_tag(game, "ECO");
        let existing_opening = meaningful_tag(game, "Opening");

        if !overwrite {
            match (existing_eco, existing_opening) {
                (Some(_), Some(_)) => return ClassifyOutcome::PreservedExisting,
                (Some(eco), None) => {
                    if eco.eq_ignore_ascii_case(&entry.eco) {
                        game.set_header("Opening", entry.name.clone());
                        return ClassifyOutcome::Updated;
                    }
                    return ClassifyOutcome::PreservedExisting;
                }
                (None, Some(_)) => return ClassifyOutcome::PreservedExisting,
                (None, None) => {}
            }
        }

        game.set_header("ECO", entry.eco.clone());
        game.set_header("Opening", entry.name.clone());
        ClassifyOutcome::Updated
    }
}

fn prefers_entry(candidate: &EcoEntry, existing: &EcoEntry) -> bool {
    (
        candidate.plies,
        candidate.eco.as_str(),
        candidate.name.as_str(),
    ) < (
        existing.plies,
        existing.eco.as_str(),
        existing.name.as_str(),
    )
}

fn meaningful_tag<'a>(game: &'a Game, key: &str) -> Option<&'a str> {
    game.header(key)
        .map(str::trim)
        .filter(|val| !val.is_empty() && *val != "?")
}

fn is_standard_start(game: &Game, root_pos: &Chess) -> bool {
    if let Some(variant) = game.header("Variant").map(str::trim) {
        let lower = variant.to_ascii_lowercase();
        if !lower.is_empty()
            && !matches!(
                lower.as_str(),
                "standard" | "chess" | "normal" | "classical" | "classic"
            )
        {
            return false;
        }
    }
    if root_pos.castles().mode() != CastlingMode::Standard {
        return false;
    }
    if game.errors.iter().any(|err| err.contains("invalid FEN")) {
        return false;
    }
    let default_pos = Chess::default();
    if position::position_key(root_pos) != position::position_key(&default_pos) {
        return false;
    }
    if let Some(fen) = game.header("FEN").map(str::trim)
        && !fen.is_empty()
        && !fen.starts_with("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -")
    {
        return false;
    }
    true
}

fn replay_defining_pgn(pgn: &str) -> Option<(u64, u16)> {
    let mut pos = Chess::default();
    let mut plies = 0u16;
    for raw_token in pgn.split_whitespace() {
        let token = match raw_token.rfind('.') {
            Some(dot_idx) => &raw_token[dot_idx + 1..],
            None => raw_token,
        };
        if token.is_empty()
            || matches!(token, "*" | "1-0" | "0-1" | "1/2-1/2")
            || token.starts_with('$')
        {
            continue;
        }
        let san_plus = SanPlus::from_ascii(token.as_bytes()).ok()?;
        let mv = san_plus.san.to_move(&pos).ok()?;
        pos = pos.play(mv).ok()?;
        plies = plies.saturating_add(1);
    }
    (plies > 0).then(|| (position::position_key(&pos), plies))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;

    #[test]
    fn embedded_dataset_loads_all_five_eco_volumes() {
        let table = global_table();
        assert!(
            table.len() > 3_400,
            "expected > 3,400 unique opening positions, got {}",
            table.len()
        );
    }

    #[test]
    fn classifies_najdorf_ruy_lopez_and_queens_gambit_from_mainline() {
        let najdorf =
            parse_pgn(b"1. e4 c5 2. Nf3 d6 3. d4 cxd4 4. Nxd4 Nf6 5. Nc3 a6 (5... g6) *\n")
                .unwrap()
                .remove(0);
        let entry = classify_game(&najdorf).expect("Najdorf should be classified");
        assert_eq!(entry.eco, "B90");
        assert_eq!(entry.name, "Sicilian Defense: Najdorf Variation");

        let english_attack = parse_pgn(
            b"1. e4 c5 2. Nf3 d6 3. d4 cxd4 4. Nxd4 Nf6 5. Nc3 a6 (5... g6) 6. Be3 e5 *\n",
        )
        .unwrap()
        .remove(0);
        let entry = classify_game(&english_attack).expect("English Attack should be classified");
        assert_eq!(entry.eco, "B90");
        assert_eq!(
            entry.name,
            "Sicilian Defense: Najdorf Variation, English Attack"
        );

        let ruy_lopez = parse_pgn(b"1. e4 e5 2. Nf3 Nc6 3. Bb5 *\n")
            .unwrap()
            .remove(0);
        let entry = classify_game(&ruy_lopez).expect("Ruy Lopez should be classified");
        assert_eq!(entry.eco, "C60");
        assert_eq!(entry.name, "Ruy Lopez");

        let qg = parse_pgn(b"1. d4 d5 2. c4 *\n").unwrap().remove(0);
        let entry = classify_game(&qg).expect("Queen's Gambit should be classified");
        assert_eq!(entry.eco, "D06");
        assert_eq!(entry.name, "Queen's Gambit");
    }

    #[test]
    fn resolves_transpositions_deterministically() {
        // Reaching the Queen's Gambit Declined via the English move order
        // (1. c4 e6 2. Nc3 d5 3. d4) must classify to the exact same entry as
        // 1. d4 d5 2. c4 e6 3. Nc3.
        let english_order = parse_pgn(b"1. c4 e6 2. Nc3 d5 3. d4 *\n")
            .unwrap()
            .remove(0);
        let standard_order = parse_pgn(b"1. d4 d5 2. c4 e6 3. Nc3 *\n")
            .unwrap()
            .remove(0);
        let from_english = classify_game(&english_order).unwrap();
        let from_standard = classify_game(&standard_order).unwrap();
        assert_eq!(from_english, from_standard);
        assert_eq!(from_english.eco, "D31");

        // Tie-breaking when two custom TSV rows reach the exact same position:
        // shortest defining line wins, then lexicographically smallest ECO and name.
        let custom_tsv = "\
eco\tname\tpgn
C99\tLonger Path\t1. e4 e5 2. Nf3 Nc6 3. Bd3 Bc5 4. Bc4
C50\tItalian Game\t1. e4 e5 2. Nf3 Nc6 3. Bc4
C51\tAlternate Name\t1. e4 e5 2. Bc4 Nc6 3. Nf3
";
        let table = EcoTable::from_tsv_sources(&[custom_tsv]);
        let game = parse_pgn(b"1. e4 e5 2. Nf3 Nc6 3. Bc4 *\n")
            .unwrap()
            .remove(0);
        let matched = table.classify_game(&game).unwrap();
        assert_eq!(matched.eco, "C50");
        assert_eq!(matched.name, "Italian Game");
        assert_eq!(matched.plies, 5);
    }

    #[test]
    fn custom_start_chess960_and_illegal_first_move_remain_unclassified() {
        let custom_start = parse_pgn(
            br#"[SetUp "1"]
[FEN "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3"]

3. Bb5 *
"#,
        )
        .unwrap()
        .remove(0);
        assert_eq!(classify_game(&custom_start), None);

        let chess960 = parse_pgn(
            br#"[Variant "Chess960"]
[SetUp "1"]
[FEN "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 *
"#,
        )
        .unwrap()
        .remove(0);
        assert_eq!(classify_game(&chess960), None);

        let illegal_start = parse_pgn(b"1. Qxe8 *\n").unwrap().remove(0);
        assert_eq!(classify_game(&illegal_start), None);

        // An illegal move after a valid opening prefix stops cleanly and keeps
        // the deepest valid opening position before the error.
        let illegal_later = parse_pgn(b"1. e4 e5 2. Nf3 Nc6 3. Bb5 Qxe8 *\n")
            .unwrap()
            .remove(0);
        let entry = classify_game(&illegal_later).unwrap();
        assert_eq!(entry.eco, "C60");
        assert_eq!(entry.name, "Ruy Lopez");
    }

    #[test]
    fn apply_classification_preserves_or_overwrites_existing_tags() {
        let mut game = parse_pgn(
            br#"[Event "Test"]
[ECO "A00"]
[Opening "Custom Tag"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 *
"#,
        )
        .unwrap()
        .remove(0);

        assert_eq!(
            apply_classification(&mut game, false),
            ClassifyOutcome::PreservedExisting
        );
        assert_eq!(game.header("ECO"), Some("A00"));
        assert_eq!(game.header("Opening"), Some("Custom Tag"));

        assert_eq!(
            apply_classification(&mut game, true),
            ClassifyOutcome::Updated
        );
        assert_eq!(game.header("ECO"), Some("C60"));
        assert_eq!(game.header("Opening"), Some("Ruy Lopez"));

        // Placeholder "?" tags are treated as absent even when overwrite is false.
        let mut placeholder_game = parse_pgn(
            br#"[Event "Test"]
[ECO "?"]
[Opening "?"]

1. d4 d5 2. c4 *
"#,
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            apply_classification(&mut placeholder_game, false),
            ClassifyOutcome::Updated
        );
        assert_eq!(placeholder_game.header("ECO"), Some("D06"));
        assert_eq!(placeholder_game.header("Opening"), Some("Queen's Gambit"));
    }
}
