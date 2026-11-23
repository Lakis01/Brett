//! Clean-room MIT opening repertoire builder, Zobrist transposition explorer,
//! reference gap finder, and SM-2+ spaced-repetition training engine.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::chess::{CastlingMode, Chess, Color, Position, Square, UciMove};
use crate::domain::{Game, parse_pgn};
use crate::pgn::game_to_pgn;
use crate::position::{PositionIndex, position_key};
use crate::settings::{config_dir, read_with_backup_json, write_atomic_with_backup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecallGrade {
    Again,
    Hard,
    Good,
    Easy,
}

impl RecallGrade {
    pub const ALL: [Self; 4] = [Self::Again, Self::Hard, Self::Good, Self::Easy];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Again => "1 Again",
            Self::Hard => "2 Hard",
            Self::Good => "3 Good",
            Self::Easy => "4 Easy",
        }
    }
}

/// Persistent SM-2+ spaced-repetition scheduling state for a position + move.
#[derive(Clone, Debug, PartialEq)]
pub struct SrsCard {
    pub pos_key: u64,
    pub expected_uci: String,
    pub ease: f32,
    pub interval_days: f32,
    pub reps: u32,
    pub lapses: u32,
    pub due_day: u64,
    pub last_reviewed_day: Option<u64>,
}

impl SrsCard {
    #[must_use]
    pub fn new(pos_key: u64, expected_uci: String) -> Self {
        Self {
            pos_key,
            expected_uci,
            ease: 2.5,
            interval_days: 0.0,
            reps: 0,
            lapses: 0,
            due_day: 0,
            last_reviewed_day: None,
        }
    }

    #[must_use]
    pub fn key(pos_key: u64, uci: &str) -> String {
        format!("{pos_key:016x}:{uci}")
    }
}

/// Current epoch day (UTC days since 1970-01-01).
#[must_use]
pub fn today_epoch_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

/// Apply a clean-room SM-2+ recall update for `grade` on `today_day`.
pub fn review_card(card: &mut SrsCard, grade: RecallGrade, today_day: u64) {
    match grade {
        RecallGrade::Again => {
            card.reps = 0;
            card.lapses += 1;
            card.ease = (card.ease - 0.20).max(1.3);
            card.interval_days = 0.0;
            card.due_day = today_day;
        }
        RecallGrade::Hard => {
            card.ease = (card.ease - 0.15).max(1.3);
            card.interval_days = if card.reps == 0 {
                1.0
            } else {
                (card.interval_days * 1.2).max(1.0)
            };
            card.reps += 1;
            card.due_day = today_day + card.interval_days.round() as u64;
        }
        RecallGrade::Good => {
            card.interval_days = match card.reps {
                0 => 1.0,
                1 => 6.0,
                _ => (card.interval_days * card.ease).max(1.0),
            };
            card.reps += 1;
            card.due_day = today_day + card.interval_days.round() as u64;
        }
        RecallGrade::Easy => {
            card.ease = (card.ease + 0.15).min(3.0);
            card.interval_days = match card.reps {
                0 => 4.0,
                1 => 10.0,
                _ => (card.interval_days * card.ease * 1.3).max(1.0),
            };
            card.reps += 1;
            card.due_day = today_day + card.interval_days.round() as u64;
        }
    }
    card.last_reviewed_day = Some(today_day);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repertoire {
    pub id: String,
    pub name: String,
    pub color: Color,
    pub pgn: String,
    pub cards: BTreeMap<String, SrsCard>,
}

impl Repertoire {
    #[must_use]
    pub fn new(id: String, name: String, color: Color) -> Self {
        let mut game = Game::default();
        game.set_header("Event", name.clone());
        Self {
            id: sanitize_id(&id),
            name,
            color,
            pgn: game_to_pgn(&game),
            cards: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn parse_game(&self) -> Game {
        let mut parsed = parse_pgn(self.pgn.as_bytes()).unwrap_or_default();
        parsed.drain(..).next().unwrap_or_default()
    }

    pub fn set_game(&mut self, game: &Game) {
        self.pgn = game_to_pgn(game);
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let cards: Vec<serde_json::Value> = self
            .cards
            .iter()
            .map(|(k, c)| {
                serde_json::json!({
                    "key": k,
                    "pos_key": format!("{:016x}", c.pos_key),
                    "expected_uci": c.expected_uci,
                    "ease": c.ease,
                    "interval_days": c.interval_days,
                    "reps": c.reps,
                    "lapses": c.lapses,
                    "due_day": c.due_day,
                    "last_reviewed_day": c.last_reviewed_day,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "id": self.id,
            "name": self.name,
            "color": if self.color == Color::White { "white" } else { "black" },
            "pgn": self.pgn,
            "cards": cards,
        }))
        .unwrap_or_default()
    }

    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let val: serde_json::Value = serde_json::from_str(text).ok()?;
        let id = sanitize_id(val.get("id")?.as_str()?);
        if id.is_empty() {
            return None;
        }
        let name = val
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&id)
            .to_string();
        let color = match val
            .get("color")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("white")
            .to_ascii_lowercase()
            .as_str()
        {
            "black" | "b" => Color::Black,
            _ => Color::White,
        };
        let pgn = val
            .get("pgn")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut cards = BTreeMap::new();
        if let Some(arr) = val.get("cards").and_then(serde_json::Value::as_array) {
            for item in arr {
                let Some(expected_uci) = item
                    .get("expected_uci")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                else {
                    continue;
                };
                let pos_key = item
                    .get("pos_key")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    .unwrap_or(0);
                let key = item
                    .get("key")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| SrsCard::key(pos_key, &expected_uci));
                let ease = item
                    .get("ease")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(2.5)
                    .clamp(1.3, 3.0) as f32;
                let interval_days = item
                    .get("interval_days")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0)
                    .max(0.0) as f32;
                let reps = item
                    .get("reps")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32;
                let lapses = item
                    .get("lapses")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32;
                let due_day = item
                    .get("due_day")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let last_reviewed_day = item
                    .get("last_reviewed_day")
                    .and_then(serde_json::Value::as_u64);
                cards.insert(
                    key,
                    SrsCard {
                        pos_key,
                        expected_uci,
                        ease,
                        interval_days,
                        reps,
                        lapses,
                        due_day,
                        last_reviewed_day,
                    },
                );
            }
        }
        Some(Self {
            id,
            name,
            color,
            pgn,
            cards,
        })
    }
}

#[must_use]
pub fn sanitize_id(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = clean.trim_matches('-');
    if trimmed.is_empty() {
        "repertoire".to_string()
    } else {
        trimmed.to_string()
    }
}

#[must_use]
pub fn unique_id_for_name(name: &str, existing: &[Repertoire]) -> String {
    let base = sanitize_id(&name.to_ascii_lowercase().replace(' ', "-"));
    if !existing.iter().any(|r| r.id == base) {
        return base;
    }
    for n in 2..=999 {
        let cand = format!("{base}-{n}");
        if !existing.iter().any(|r| r.id == cand) {
            return cand;
        }
    }
    format!("{base}-{}", today_epoch_day())
}

#[must_use]
pub fn repertoires_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("repertoires"))
}

pub fn list_repertoires_in(dir: &Path) -> Vec<Repertoire> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && let Some(text) = read_with_backup_json(&path)
            && let Some(rep) = Repertoire::from_json(&text)
        {
            out.push(rep);
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

#[must_use]
pub fn list_repertoires() -> Vec<Repertoire> {
    repertoires_dir().map_or_else(Vec::new, |d| list_repertoires_in(&d))
}

pub fn save_repertoire_to(dir: &Path, rep: &Repertoire) -> Result<()> {
    let id = sanitize_id(&rep.id);
    if id.is_empty() {
        bail!("Repertoire id cannot be empty");
    }
    let path = dir.join(format!("{id}.json"));
    write_atomic_with_backup(&path, &rep.to_json())
        .with_context(|| format!("saving repertoire {}", path.display()))
}

pub fn save_repertoire(rep: &Repertoire) -> Result<()> {
    let Some(dir) = repertoires_dir() else {
        return Ok(());
    };
    save_repertoire_to(&dir, rep)
}

pub fn delete_repertoire_in(dir: &Path, id: &str) -> Result<()> {
    let path = dir.join(format!("{}.json", sanitize_id(id)));
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    let bak = path.with_extension("json.bak");
    if bak.exists() {
        let _ = fs::remove_file(bak);
    }
    Ok(())
}

pub fn delete_repertoire(id: &str) -> Result<()> {
    let Some(dir) = repertoires_dir() else {
        return Ok(());
    };
    delete_repertoire_in(&dir, id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepertoireMoveInfo {
    pub node: usize,
    pub uci: String,
    pub san: String,
    pub comment: String,
}

/// Group all repertoire moves across every branch by parent Zobrist position
/// hash so transpositions show up together automatically.
#[must_use]
pub fn position_moves_map(game: &Game) -> HashMap<u64, Vec<RepertoireMoveInfo>> {
    let mut map: HashMap<u64, Vec<RepertoireMoveInfo>> = HashMap::new();
    for node in &game.nodes {
        if node.children.is_empty() {
            continue;
        }
        let key = position_key(&node.pos);
        let entry = map.entry(key).or_default();
        for &child_idx in &node.children {
            let Some(child) = game.nodes.get(child_idx) else {
                continue;
            };
            let Some(ply) = &child.mv else {
                continue;
            };
            if !entry.iter().any(|existing| existing.uci == ply.uci) {
                entry.push(RepertoireMoveInfo {
                    node: child_idx,
                    uci: ply.uci.clone(),
                    san: ply.san.clone(),
                    comment: child.comment.clone(),
                });
            }
        }
    }
    map
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapReport {
    pub path_nodes: Vec<usize>,
    pub opponent_uci: String,
    pub opponent_san: String,
    pub reference_games: u32,
    pub existing_opponent_node: Option<usize>,
}

/// Find the highest-frequency opponent move in `reference` where the repertoire
/// reaches a position with `color.other()` to move and lacks a prepared answer
/// for `color` (including via transpositions).
#[must_use]
pub fn find_biggest_gap(
    game: &Game,
    color: Color,
    reference: &PositionIndex,
    min_games: u32,
) -> Option<GapReport> {
    let transpositions = position_moves_map(game);
    let mut best: Option<GapReport> = None;
    let mut visited_keys = HashSet::new();
    let mut queue = VecDeque::new();
    if !game.nodes.is_empty() {
        queue.push_back((0usize, vec![0usize]));
    }

    while let Some((node_idx, path)) = queue.pop_front() {
        let Some(node) = game.nodes.get(node_idx) else {
            continue;
        };
        let key = position_key(&node.pos);
        if !visited_keys.insert((key, node.pos.turn())) {
            continue;
        }

        if node.pos.turn() == color.other() {
            let stats = reference.moves(key);
            for stat in stats {
                if stat.games < min_games {
                    continue;
                }
                let Some(mv) = stat.to_move(&node.pos) else {
                    continue;
                };
                let opp_uci = UciMove::from_move(mv, CastlingMode::Standard).to_string();
                let opp_san = stat.san(&node.pos).unwrap_or_else(|| opp_uci.clone());
                let existing_child = node.children.iter().copied().find(|&c| {
                    game.nodes[c]
                        .mv
                        .as_ref()
                        .is_some_and(|ply| ply.uci == opp_uci)
                });
                let has_response = if let Some(child_id) = existing_child {
                    let child_pos = &game.nodes[child_id].pos;
                    let child_key = position_key(child_pos);
                    !game.nodes[child_id].children.is_empty()
                        || transpositions
                            .get(&child_key)
                            .is_some_and(|v| !v.is_empty())
                } else {
                    false
                };
                if !has_response
                    && best
                        .as_ref()
                        .is_none_or(|cur| stat.games > cur.reference_games)
                {
                    let mut full_path = path.clone();
                    if let Some(child_id) = existing_child {
                        full_path.push(child_id);
                    }
                    best = Some(GapReport {
                        path_nodes: full_path,
                        opponent_uci: opp_uci,
                        opponent_san: opp_san,
                        reference_games: stat.games,
                        existing_opponent_node: existing_child,
                    });
                }
            }
        }

        for &child in &node.children {
            let mut next_path = path.clone();
            next_path.push(child);
            queue.push_back((child, next_path));
        }
    }

    best
}

#[derive(Clone, Debug)]
pub struct DrillItem {
    pub pos_key: u64,
    pub node_id: usize,
    pub path_nodes: Vec<usize>,
    pub prompt_pos: Chess,
    pub expected_ucis: Vec<String>,
    pub expected_sans: Vec<String>,
    pub primary_uci: String,
    pub card_key: String,
    pub due_day: u64,
}

/// Build an ordered list of drill prompts for `color` from `game`. When
/// `due_only` is true, returns only cards due on or before `today_day`.
#[must_use]
pub fn build_drill_queue(
    game: &Game,
    color: Color,
    cards: &BTreeMap<String, SrsCard>,
    today_day: u64,
    due_only: bool,
) -> Vec<DrillItem> {
    let transpositions = position_moves_map(game);
    let mut seen_keys = HashSet::new();
    let mut items = Vec::new();
    let mut queue = VecDeque::new();
    if !game.nodes.is_empty() {
        queue.push_back((0usize, vec![0usize]));
    }

    while let Some((node_idx, path)) = queue.pop_front() {
        let Some(node) = game.nodes.get(node_idx) else {
            continue;
        };
        if node.pos.turn() == color
            && !node.children.is_empty()
            && let Some(moves) = transpositions.get(&position_key(&node.pos))
            && !moves.is_empty()
        {
            let key = position_key(&node.pos);
            if seen_keys.insert(key) {
                let expected_ucis: Vec<String> = moves.iter().map(|m| m.uci.clone()).collect();
                let expected_sans: Vec<String> = moves.iter().map(|m| m.san.clone()).collect();
                let primary_uci = expected_ucis[0].clone();
                let card_key = SrsCard::key(key, &primary_uci);
                let due_day = cards.get(&card_key).map_or(0, |c| c.due_day);
                if !due_only || due_day <= today_day {
                    items.push(DrillItem {
                        pos_key: key,
                        node_id: node_idx,
                        path_nodes: path.clone(),
                        prompt_pos: node.pos.clone(),
                        expected_ucis,
                        expected_sans,
                        primary_uci,
                        card_key,
                        due_day,
                    });
                }
            }
        }
        for &child in &node.children {
            let mut next_path = path.clone();
            next_path.push(child);
            queue.push_back((child, next_path));
        }
    }

    items.sort_by_key(|item| (item.due_day, item.path_nodes.len(), item.node_id));
    items
}

#[derive(Clone, Debug)]
pub struct TrainSession {
    pub repertoire_id: String,
    pub color: Color,
    pub queue: Vec<DrillItem>,
    pub index: usize,
    pub attempts: u32,
    pub revealed: bool,
    pub awaiting_grade: bool,
    pub last_feedback: String,
    pub completed: usize,
    pub correct_first_try: usize,
    pub hint_square: Option<Square>,
}

impl TrainSession {
    #[must_use]
    pub fn new(repertoire_id: String, color: Color, queue: Vec<DrillItem>) -> Self {
        Self {
            repertoire_id,
            color,
            queue,
            index: 0,
            attempts: 0,
            revealed: false,
            awaiting_grade: false,
            last_feedback: "Play your repertoire move on the board.".to_string(),
            completed: 0,
            correct_first_try: 0,
            hint_square: None,
        }
    }

    #[must_use]
    pub fn current_item(&self) -> Option<&DrillItem> {
        self.queue.get(self.index)
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.index >= self.queue.len()
    }

    /// Validate a move played by the user on the board against every
    /// transposition-valid move at this position.
    pub fn submit_move(&mut self, mv_uci: &str) -> bool {
        let Some(item) = self.queue.get(self.index) else {
            return false;
        };
        if item.expected_ucis.iter().any(|u| u == mv_uci) {
            if self.attempts == 0 && !self.revealed {
                self.correct_first_try += 1;
            }
            self.awaiting_grade = true;
            self.hint_square = None;
            self.last_feedback = format!(
                "Correct ({}) — rate recall (1 Again · 2 Hard · 3 Good · 4 Easy).",
                item.expected_sans.join(" / ")
            );
            true
        } else {
            self.attempts += 1;
            self.last_feedback = format!("'{mv_uci}' is not in your repertoire here. Try again.");
            false
        }
    }

    pub fn request_hint(&mut self) {
        if let Some(item) = self.current_item()
            && let Ok(uci) = UciMove::from_ascii(item.primary_uci.as_bytes())
        {
            self.hint_square = uci.from();
            self.last_feedback = "Hint: highlighted the piece to move.".to_string();
        }
    }

    pub fn reveal(&mut self) {
        if let Some(item) = self.current_item() {
            let answer = item.expected_sans.join(" / ");
            self.revealed = true;
            self.awaiting_grade = true;
            self.hint_square = None;
            self.last_feedback = format!("Answer: {answer}. Rate recall (1 Again … 4 Easy).");
        }
    }

    /// Grade the current card, persist its updated schedule into `cards`, and
    /// advance to the next drill item (re-queuing `Again` within this session).
    pub fn apply_grade(
        &mut self,
        grade: RecallGrade,
        cards: &mut BTreeMap<String, SrsCard>,
        today_day: u64,
    ) -> bool {
        let Some(item) = self.queue.get(self.index).cloned() else {
            return false;
        };
        let card = cards
            .entry(item.card_key.clone())
            .or_insert_with(|| SrsCard::new(item.pos_key, item.primary_uci.clone()));
        review_card(card, grade, today_day);
        if grade == RecallGrade::Again {
            self.queue.push(item);
        } else {
            self.completed += 1;
        }
        self.index += 1;
        self.attempts = 0;
        self.revealed = false;
        self.awaiting_grade = false;
        self.hint_square = None;
        self.last_feedback = if self.is_finished() {
            "Training session complete!".to_string()
        } else {
            "Play your repertoire move on the board.".to_string()
        };
        true
    }
}

/// Merge multiple PGN games (e.g. multi-chapter Lichess Studies) into one
/// opening repertoire tree, preserving comments, NAGs, and shapes and combining
/// shared opening prefixes without duplicating identical moves.
#[must_use]
pub fn merge_pgn_games(games: Vec<Game>) -> Game {
    fn merge_subtree(dst: &mut Game, dst_parent: usize, src: &Game, src_node: usize) {
        let src_children = src.nodes[src_node].children.clone();
        for src_child in src_children {
            let child = &src.nodes[src_child];
            let existing = dst.nodes[dst_parent].children.iter().copied().find(|&c| {
                dst.nodes[c]
                    .mv
                    .as_ref()
                    .zip(child.mv.as_ref())
                    .is_some_and(|(a, b)| a.uci == b.uci && a.san == b.san)
            });
            let dst_child = if let Some(idx) = existing {
                if !child.comment.is_empty() && !dst.nodes[idx].comment.contains(&child.comment) {
                    if !dst.nodes[idx].comment.is_empty() {
                        dst.nodes[idx].comment.push('\n');
                    }
                    dst.nodes[idx].comment.push_str(&child.comment);
                }
                for &nag in &child.nags {
                    if !dst.nodes[idx].nags.contains(&nag) {
                        dst.nodes[idx].nags.push(nag);
                    }
                }
                for &shape in &child.shapes {
                    if !dst.nodes[idx].shapes.contains(&shape) {
                        dst.nodes[idx].shapes.push(shape);
                    }
                }
                if dst.nodes[idx].eval.is_none() {
                    dst.nodes[idx].eval = child.eval;
                }
                if dst.nodes[idx].clock.is_none() {
                    dst.nodes[idx].clock = child.clock;
                }
                idx
            } else {
                let mut cloned: crate::domain::Node = child.clone();
                cloned.parent = Some(dst_parent);
                cloned.children = Vec::new();
                let new_idx = dst.nodes.len();
                dst.nodes.push(cloned);
                dst.nodes[dst_parent].children.push(new_idx);
                new_idx
            };
            merge_subtree(dst, dst_child, src, src_child);
        }
    }

    let mut iter = games.into_iter();
    let Some(mut merged) = iter.next() else {
        return Game::default();
    };
    let root_key = position_key(&merged.nodes[0].pos);
    for chapter in iter {
        if position_key(&chapter.nodes[0].pos) != root_key {
            continue;
        }
        let ch_root_comment = chapter.nodes[0].comment.trim();
        if !ch_root_comment.is_empty() && !merged.nodes[0].comment.contains(ch_root_comment) {
            if !merged.nodes[0].comment.is_empty() {
                merged.nodes[0].comment.push('\n');
            }
            merged.nodes[0].comment.push_str(ch_root_comment);
        }
        merge_subtree(&mut merged, 0, &chapter, 0);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::BuildOptions;

    #[test]
    fn sm2_plus_review_grades_update_intervals_and_ease() {
        let today = 20_000;
        let mut card = SrsCard::new(0xdeadbeef, "e2e4".to_string());

        review_card(&mut card, RecallGrade::Good, today);
        assert_eq!(card.reps, 1);
        assert_eq!(card.interval_days, 1.0);
        assert_eq!(card.due_day, today + 1);

        review_card(&mut card, RecallGrade::Good, today + 1);
        assert_eq!(card.reps, 2);
        assert_eq!(card.interval_days, 6.0);
        assert_eq!(card.due_day, today + 7);

        review_card(&mut card, RecallGrade::Easy, today + 7);
        assert_eq!(card.reps, 3);
        assert!(card.interval_days > 6.0);
        assert!(card.ease > 2.5);

        review_card(&mut card, RecallGrade::Again, today + 20);
        assert_eq!(card.reps, 0);
        assert_eq!(card.lapses, 1);
        assert_eq!(card.due_day, today + 20);

        review_card(&mut card, RecallGrade::Hard, today + 20);
        assert_eq!(card.reps, 1);
        assert_eq!(card.interval_days, 1.0);
    }

    #[test]
    fn zobrist_transposition_map_groups_moves_from_different_move_orders() {
        let g2 = parse_pgn(b"1. d4 Nf6 (1... e6 2. c4 Nf6 3. Nf3 d5) 2. c4 e6 3. Nc3 Bb4 *")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let map2 = position_moves_map(&g2);
        let node_before_nc3 = g2
            .nodes
            .iter()
            .find(|n| {
                n.children
                    .iter()
                    .any(|&c| g2.nodes[c].mv.as_ref().is_some_and(|p| p.uci == "b1c3"))
            })
            .expect("node before 3. Nc3");
        let key_after_e6_nf6 = position_key(&node_before_nc3.pos);
        let moves = map2.get(&key_after_e6_nf6).unwrap();
        let ucis: Vec<&str> = moves.iter().map(|m| m.uci.as_str()).collect();
        // Both Nc3 (from 1.d4 Nf6 2.c4 e6) and Nf3 (from 1.d4 e6 2.c4 Nf6) share the same Zobrist key.
        assert!(ucis.contains(&"b1c3"));
        assert!(ucis.contains(&"g1f3"));
    }

    #[test]
    fn find_biggest_gap_identifies_most_popular_unanswered_reply() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp_dir =
            std::env::temp_dir().join(format!("brett-rep-gap-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let pgn_path = tmp_dir.join("ref.pgn");
        let idx_path = tmp_dir.join("ref.brettpos");

        let ref_pgn = b"\
[White \"A\"][Black \"B\"]\n\n1. e4 c5 2. Nf3 d6 1-0\n\n\
[White \"A\"][Black \"B\"]\n\n1. e4 c5 2. Nf3 d6 1-0\n\n\
[White \"A\"][Black \"B\"]\n\n1. e4 c5 2. Nf3 d6 1-0\n\n\
[White \"A\"][Black \"B\"]\n\n1. e4 e5 2. Nf3 Nc6 1-0\n";
        fs::write(&pgn_path, ref_pgn).unwrap();
        PositionIndex::build_to(&pgn_path, &idx_path, BuildOptions::default(), |_| {}).unwrap();
        let index = PositionIndex::open(&idx_path).unwrap();

        // White repertoire covers 1. e4 e5 2. Nf3, leaving 1... c5 unanswered (3 ref games).
        let rep_game = parse_pgn(b"1. e4 e5 2. Nf3 *")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let gap = find_biggest_gap(&rep_game, Color::White, &index, 1).unwrap();
        assert_eq!(gap.opponent_uci, "c7c5");
        assert_eq!(gap.reference_games, 3);
        assert!(gap.existing_opponent_node.is_none());

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn drill_queue_and_train_session_persist_round_trip() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("brett-rep-store-{}-{nanos}", std::process::id()));
        let mut rep = Repertoire::new(
            "italian".to_string(),
            "Italian Game".to_string(),
            Color::White,
        );
        let g = parse_pgn(b"1. e4 e5 2. Nf3 Nc6 *")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        rep.set_game(&g);

        let today = 20_000;
        let queue = build_drill_queue(&g, Color::White, &rep.cards, today, true);
        assert_eq!(queue.len(), 2);

        let mut session = TrainSession::new(rep.id.clone(), Color::White, queue);
        assert!(!session.submit_move("d2d4"));
        assert_eq!(session.attempts, 1);
        assert!(session.submit_move("e2e4"));
        assert!(session.awaiting_grade);
        assert!(session.apply_grade(RecallGrade::Good, &mut rep.cards, today));

        save_repertoire_to(&dir, &rep).unwrap();
        let reloaded = list_repertoires_in(&dir);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].cards.len(), 1);
        assert_eq!(reloaded[0].name, "Italian Game");

        let due_after = build_drill_queue(&g, Color::White, &reloaded[0].cards, today, true);
        assert_eq!(due_after.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_pgn_games_combines_multiple_chapters_into_one_tree() {
        let ch1 =
            parse_pgn(b"[Event \"Italian Study\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 {Main line} *")
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
        let ch2 = parse_pgn(
            b"[Event \"Italian Study Ch 2\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 {Ruy Lopez} a6 *",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        let ch3 = parse_pgn(
            b"[Event \"Italian Study Ch 3\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bc4 Nf6 {Two Knights} *",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        let merged = merge_pgn_games(vec![ch1, ch2, ch3]);
        // At 1. e4 e5 2. Nf3 Nc6 (node index 4), White should have both 3. Bc4 and 3. Bb5.
        let ply4 = merged.mainline()[3];
        assert_eq!(merged.nodes[ply4].children.len(), 2);
        let c4 = merged.nodes[ply4].children[0];
        let b5 = merged.nodes[ply4].children[1];
        assert_eq!(merged.nodes[c4].mv.as_ref().unwrap().san, "Bc4");
        assert_eq!(merged.nodes[b5].mv.as_ref().unwrap().san, "Bb5");
        // After 3. Bc4, Black has both 3... Bc5 and 3... Nf6.
        assert_eq!(merged.nodes[c4].children.len(), 2);
    }
}
