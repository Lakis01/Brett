//! Sequential game review on top of the UCI engine.
//!
//! The review analyzes every mainline position once. A move is graded by
//! comparing the engine's evaluation before the move with the evaluation after
//! the played move, both from the mover's point of view. This is deliberately
//! a display-oriented, engine-relative grade; it is not a claim of objective
//! accuracy.

use std::path::PathBuf;
use std::time::Instant;

use crate::chess::{Color, EnPassantMode, Fen, Position};

use crate::domain::Game;
use crate::engine::{EngineHandle, EngineInfo, EngineOptions, Score, SearchLimit};
use crate::eval::{self, WhiteScore};

/// A coarse move-quality label shown in the review table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Best,
    Excellent,
    Good,
    Inaccuracy,
    Mistake,
    Blunder,
    Forced,
    Unknown,
}

impl Quality {
    pub const ALL: [Self; 8] = [
        Self::Best,
        Self::Excellent,
        Self::Good,
        Self::Inaccuracy,
        Self::Mistake,
        Self::Blunder,
        Self::Forced,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Best => "Best",
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Inaccuracy => "Inaccuracy",
            Self::Mistake => "Mistake",
            Self::Blunder => "Blunder",
            Self::Forced => "Forced",
            Self::Unknown => "Unrated",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Best => "★",
            Self::Excellent => "!",
            Self::Good => "✓",
            Self::Inaccuracy => "?!",
            Self::Mistake => "?",
            Self::Blunder => "??",
            Self::Forced => "□",
            Self::Unknown => "·",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Best => 0,
            Self::Excellent => 1,
            Self::Good => 2,
            Self::Inaccuracy => 3,
            Self::Mistake => 4,
            Self::Blunder => 5,
            Self::Forced => 6,
            Self::Unknown => 7,
        }
    }
}

/// Search settings for one review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewConfig {
    pub limit: SearchLimit,
}

/// Identity of a completed review that may be reused.
///
/// The mainline is stored exactly, including node indices, so a cached review
/// can only be restored into a game with the same tree layout. Engine path,
/// resource options, search limit, and the detected opening-book prefix are
/// part of the identity because each can change the grades.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewKey {
    mainline: Vec<(usize, String)>,
    engine_path: PathBuf,
    engine_options: EngineOptions,
    limit: SearchLimit,
    book_plies: usize,
}

impl ReviewKey {
    pub fn new(
        game: &Game,
        config: ReviewConfig,
        book_plies: usize,
        engine_path: impl Into<PathBuf>,
        engine_options: EngineOptions,
    ) -> Self {
        let mainline = game.mainline();
        let book_plies = book_plies.min(mainline.len());
        let positions = mainline
            .into_iter()
            .map(|node| {
                let position = &game.nodes[node].pos;
                (
                    node,
                    Fen::from_position(position, EnPassantMode::Legal).to_string(),
                )
            })
            .collect();
        Self {
            mainline: positions,
            engine_path: engine_path.into(),
            engine_options,
            limit: config.limit,
            book_plies,
        }
    }
}

/// Engine result for a single position.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluated {
    /// UCI score from the side to move.
    pub eval: Option<Score>,
    /// Engine's preferred move in UCI notation.
    pub best: Option<String>,
    /// White-perspective centipawn value, with mates compressed near ±10000.
    pub white_cp: Option<f64>,
}

#[derive(Clone, Debug)]
struct ReviewPosition {
    fen: String,
    turn: Color,
}

/// One graded mainline move.
#[derive(Clone, Debug)]
pub struct MoveReview {
    pub node: usize,
    pub number: u32,
    /// Side that made the move.
    pub color: Color,
    pub san: String,
    pub played: String,
    /// Score before the move, from the mover's side to move.
    pub eval_before: Option<Score>,
    /// Score after the move, from the opponent's side to move.
    pub eval_after: Option<Score>,
    pub best: Option<String>,
    /// Centipawn loss from the mover's point of view.
    pub cp_loss: Option<i32>,
    /// Mover's win probability before the move (`0.0..=100.0`).
    pub win_before: Option<f64>,
    /// Mover's win probability after the move (`0.0..=100.0`).
    pub win_after: Option<f64>,
    /// Per-move Lichess accuracy (`0.0..=100.0`); `None` for forced moves.
    pub accuracy: Option<f64>,
    pub quality: Quality,
}

/// Per-color game accuracy and average centipawn loss summary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AccuracySummary {
    pub white: Option<f64>,
    pub black: Option<f64>,
    pub white_acpl: Option<f64>,
    pub black_acpl: Option<f64>,
    pub white_moves: usize,
    pub black_moves: usize,
}

/// In-progress or completed review of a game.
#[derive(Clone)]
pub struct ReviewState {
    /// Cache identity, set when the review is started from the app.
    pub key: Option<ReviewKey>,
    pub config: ReviewConfig,
    pub book_plies: usize,
    pub running: bool,
    /// Request id of the position currently being analyzed.
    pub request: Option<u64>,
    pub started: Instant,
    positions: Vec<ReviewPosition>,
    nodes: Vec<usize>,
    evaluated: Vec<Option<Evaluated>>,
    next: usize,
    last_info: Option<EngineInfo>,
}

impl ReviewState {
    pub fn new(game: &Game, config: ReviewConfig, book_plies: usize) -> Self {
        let mainline = game.mainline();
        let book_plies = book_plies.min(mainline.len());
        let nodes = if mainline.is_empty() {
            vec![0usize]
        } else if book_plies >= mainline.len() {
            Vec::new()
        } else if book_plies == 0 {
            let mut n = Vec::with_capacity(mainline.len() + 1);
            n.push(0);
            n.extend(mainline);
            n
        } else {
            let mut n = Vec::with_capacity(mainline.len() - book_plies + 1);
            n.push(mainline[book_plies - 1]);
            n.extend_from_slice(&mainline[book_plies..]);
            n
        };
        let positions = nodes
            .iter()
            .map(|&node| {
                let position = &game.nodes[node].pos;
                ReviewPosition {
                    fen: Fen::from_position(position, EnPassantMode::Legal).to_string(),
                    turn: position.turn(),
                }
            })
            .collect::<Vec<_>>();
        let total = positions.len();
        Self {
            key: None,
            config,
            book_plies,
            running: total > 0,
            request: None,
            started: Instant::now(),
            positions,
            nodes,
            evaluated: vec![None; total],
            next: 0,
            last_info: None,
        }
    }

    /// Live engine info for the position currently being searched by the review.
    pub fn current_search_eval(&self) -> Option<(&str, Color, &EngineInfo)> {
        if !self.running {
            return None;
        }
        let pos = self.positions.get(self.next)?;
        let info = self.last_info.as_ref()?;
        Some((pos.fen.as_str(), pos.turn, info))
    }

    pub fn total(&self) -> usize {
        self.positions.len()
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.positions.len())
    }

    pub fn complete(&self) -> bool {
        !self.running && self.next >= self.positions.len()
    }

    /// Mark every position analyzed without an engine. Test-only: lets cache
    /// tests build a completed review cheaply.
    #[cfg(test)]
    pub(crate) fn mark_complete_for_test(&mut self) {
        self.evaluated.fill(Some(Evaluated {
            eval: None,
            best: None,
            white_cp: None,
        }));
        self.next = self.positions.len();
        self.running = false;
        self.request = None;
    }

    /// Ask the engine for the next position. Returns true when a request was
    /// queued.
    pub fn request_next(&mut self, engine: &mut EngineHandle) -> bool {
        if !self.running || self.next >= self.positions.len() {
            return false;
        }
        let position = &self.positions[self.next];
        self.request = Some(engine.analyze(position.fen.clone(), 1, self.config.limit));
        true
    }

    pub fn note_info(&mut self, request: u64, info: EngineInfo) {
        if self.request == Some(request) {
            self.last_info = Some(info);
        }
    }

    /// Consume the engine's `bestmove` for the current request. Returns true
    /// when it belonged to this review.
    pub fn finish_search(&mut self, request: u64, best: &str) -> bool {
        if self.request != Some(request) || self.next >= self.positions.len() {
            return false;
        }
        let info = self.last_info.take().unwrap_or_default();
        let position = &self.positions[self.next];
        self.evaluated[self.next] = Some(Evaluated {
            eval: info.score,
            best: (best != "(none)").then(|| best.to_string()),
            white_cp: info.score.map(|score| score_white_cp(score, position.turn)),
        });
        self.next += 1;
        self.request = None;
        if self.next >= self.positions.len() {
            self.running = false;
        }
        true
    }

    pub fn cancel(&mut self) {
        self.running = false;
        self.request = None;
    }

    /// Build move reviews from the positions analyzed so far.
    pub fn moves(&self, game: &Game) -> Vec<MoveReview> {
        let mut out = Vec::with_capacity(self.nodes.len().saturating_sub(1));
        for index in 1..self.nodes.len() {
            let node = self.nodes[index];
            let Some(ply) = game.nodes[node].mv.as_ref() else {
                continue;
            };
            // Only show completed pairs. The last analyzed position has no
            // following position yet while a review is running.
            let (Some(before), Some(after)) = (
                self.evaluated[index - 1].as_ref(),
                self.evaluated[index].as_ref(),
            ) else {
                continue;
            };
            let legal = game.nodes[self.nodes[index - 1]].pos.legal_moves().len();
            let (quality, cp_loss, win_before, win_after, accuracy) = if legal == 1 {
                (Quality::Forced, None, None, None, None)
            } else {
                let (quality, cp_loss) = classify(&ply.uci, Some(before), Some(after), ply.color);
                let (win_before, win_after, accuracy) = match (before.eval, after.eval) {
                    (Some(before_score), Some(after_score)) => {
                        let wb = WhiteScore::from_side_to_move(before_score, ply.color)
                            .win_percent_for(ply.color);
                        let wa = WhiteScore::from_side_to_move(after_score, ply.color.other())
                            .win_percent_for(ply.color);
                        (Some(wb), Some(wa), Some(eval::move_accuracy(wb, wa)))
                    }
                    _ => (None, None, None),
                };
                (quality, cp_loss, win_before, win_after, accuracy)
            };
            out.push(MoveReview {
                node,
                number: ply.number,
                color: ply.color,
                san: ply.san.clone(),
                played: ply.uci.clone(),
                eval_before: before.eval,
                eval_after: after.eval,
                best: before.best.clone(),
                cp_loss,
                win_before,
                win_after,
                accuracy,
                quality,
            });
        }
        out
    }

    /// White-perspective evaluation points for the graph, in move order.
    pub fn eval_points(&self) -> Vec<(usize, f64)> {
        self.evaluated
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                value
                    .as_ref()
                    .and_then(|value| value.white_cp)
                    .map(|cp| (index, cp))
            })
            .collect()
    }

    pub fn quality_counts(&self, moves: &[MoveReview]) -> [usize; 8] {
        let mut counts = [0usize; 8];
        for mov in moves {
            counts[mov.quality.index()] += 1;
        }
        counts
    }
}

/// Convert a side-to-move UCI score to White's perspective in centipawns.
///
/// Mates are compressed to just inside ±10000 so they always dominate normal
/// evaluations without overflowing charts.
pub fn score_white_cp(score: Score, turn: Color) -> f64 {
    let white = match score {
        Score::Cp(cp) => cp as f64,
        Score::Mate(mate) => {
            let magnitude = 10_000.0 - mate.unsigned_abs() as f64 * 10.0;
            if mate > 0 { magnitude } else { -magnitude }
        }
    };
    if turn == Color::White { white } else { -white }
}

fn advantage_for(score: Score, turn: Color, side: Color) -> f64 {
    let white = score_white_cp(score, turn);
    if side == Color::White { white } else { -white }
}

fn classify(
    played: &str,
    before: Option<&Evaluated>,
    after: Option<&Evaluated>,
    mover: Color,
) -> (Quality, Option<i32>) {
    let Some(before) = before else {
        return (Quality::Unknown, None);
    };
    let Some(after) = after else {
        return (Quality::Unknown, None);
    };
    let (Some(before_score), Some(after_score)) = (before.eval, after.eval) else {
        return (Quality::Unknown, None);
    };
    let before_advantage = advantage_for(before_score, mover, mover);
    let after_advantage = advantage_for(after_score, mover.other(), mover);
    let loss = (before_advantage - after_advantage).max(0.0);
    let cp_loss = loss.round() as i32;
    let quality = if before.best.as_deref() == Some(played) {
        Quality::Best
    } else if loss <= 20.0 {
        Quality::Excellent
    } else if loss <= 50.0 {
        Quality::Good
    } else if loss <= 100.0 {
        Quality::Inaccuracy
    } else if loss <= 250.0 {
        Quality::Mistake
    } else {
        Quality::Blunder
    };
    (quality, Some(cp_loss))
}

/// Compute Lichess-style per-color win-probability accuracy (combining the
/// volatility-weighted mean and harmonic mean) and average centipawn loss
/// (ACPL, with individual losses capped at 1000).
pub fn accuracy_summary(moves: &[MoveReview]) -> AccuracySummary {
    let mut white_positions = Vec::with_capacity(moves.len() + 1);
    for (idx, mov) in moves.iter().enumerate() {
        if idx == 0 {
            let w0 = mov
                .eval_before
                .map(|s| WhiteScore::from_side_to_move(s, mov.color).win_percent())
                .or_else(|| {
                    mov.win_before.map(|wb| {
                        if mov.color == Color::White {
                            wb
                        } else {
                            100.0 - wb
                        }
                    })
                })
                .unwrap_or(50.0);
            white_positions.push(w0);
        }
        let w_after = mov
            .eval_after
            .map(|s| WhiteScore::from_side_to_move(s, mov.color.other()).win_percent())
            .or_else(|| {
                mov.win_after.map(|wa| {
                    if mov.color == Color::White {
                        wa
                    } else {
                        100.0 - wa
                    }
                })
            })
            .or_else(|| white_positions.last().copied())
            .unwrap_or(50.0);
        white_positions.push(w_after);
    }

    let weights = volatility_weights(&white_positions, moves.len());

    let mut white_weighted = Vec::new();
    let mut black_weighted = Vec::new();
    let mut white_losses = Vec::new();
    let mut black_losses = Vec::new();

    for (idx, mov) in moves.iter().enumerate() {
        if mov.quality == Quality::Forced {
            continue;
        }
        let weight = weights.get(idx).copied().unwrap_or(1.0);
        if let Some(acc) = mov.accuracy.or_else(|| {
            let (wb, wa) = (mov.win_before?, mov.win_after?);
            Some(eval::move_accuracy(wb, wa))
        }) {
            match mov.color {
                Color::White => white_weighted.push((acc, weight)),
                Color::Black => black_weighted.push((acc, weight)),
            }
        }
        if let Some(loss) = mov.cp_loss {
            let capped = f64::from(loss.clamp(0, 1_000));
            match mov.color {
                Color::White => white_losses.push(capped),
                Color::Black => black_losses.push(capped),
            }
        }
    }

    AccuracySummary {
        white: color_accuracy(&white_weighted),
        black: color_accuracy(&black_weighted),
        white_acpl: mean(&white_losses),
        black_acpl: mean(&black_losses),
        white_moves: white_weighted.len(),
        black_moves: black_weighted.len(),
    }
}

fn volatility_weights(white_positions: &[f64], move_count: usize) -> Vec<f64> {
    if move_count == 0 || white_positions.len() < 2 {
        return Vec::new();
    }
    let window_size = (white_positions.len() / 10)
        .clamp(2, 8)
        .min(white_positions.len());
    let mut windows: Vec<&[f64]> = Vec::with_capacity(move_count);
    let leading_count = window_size.saturating_sub(2);
    for i in 0..leading_count {
        let end = (i + 2).min(white_positions.len());
        windows.push(&white_positions[..end]);
    }
    for start in 0..=white_positions.len().saturating_sub(window_size) {
        windows.push(&white_positions[start..start + window_size]);
    }
    windows
        .into_iter()
        .take(move_count)
        .map(|slice| std_dev(slice).clamp(0.5, 12.0))
        .collect()
}

fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.5;
    }
    let avg = values.iter().sum::<f64>() / values.len() as f64;
    let var = values
        .iter()
        .map(|v| {
            let d = v - avg;
            d * d
        })
        .sum::<f64>()
        / values.len() as f64;
    var.sqrt()
}

fn color_accuracy(entries: &[(f64, f64)]) -> Option<f64> {
    if entries.is_empty() {
        return None;
    }
    let weight_sum: f64 = entries.iter().map(|(_, w)| *w).sum();
    let weighted_mean = if weight_sum > 0.0 {
        entries.iter().map(|(a, w)| a * w).sum::<f64>() / weight_sum
    } else {
        entries.iter().map(|(a, _)| *a).sum::<f64>() / entries.len() as f64
    };
    let inv_sum: f64 = entries.iter().map(|(a, _)| 1.0 / a.max(1.0)).sum();
    let harmonic_mean = entries.len() as f64 / inv_sum;
    Some(((weighted_mean + harmonic_mean) / 2.0).clamp(0.0, 100.0))
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Average per-move accuracy across both colors.
pub fn accuracy(moves: &[MoveReview]) -> Option<f64> {
    let summary = accuracy_summary(moves);
    match (summary.white, summary.black) {
        (Some(w), Some(b)) => Some((w + b) / 2.0),
        (Some(w), None) => Some(w),
        (None, Some(b)) => Some(b),
        (None, None) => {
            let mut total = 0.0;
            let mut count = 0usize;
            for mov in moves {
                let Some(loss) = mov.cp_loss else {
                    continue;
                };
                total += 100.0 * (-(loss as f64) / 400.0).exp();
                count += 1;
            }
            (count > 0).then_some(total / count as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluated(cp: i32, best: &str) -> Evaluated {
        Evaluated {
            eval: Some(Score::Cp(cp)),
            best: Some(best.to_string()),
            white_cp: Some(cp as f64),
        }
    }

    #[test]
    fn white_scores_flip_for_black() {
        assert_eq!(score_white_cp(Score::Cp(40), Color::White), 40.0);
        assert_eq!(score_white_cp(Score::Cp(40), Color::Black), -40.0);
        assert_eq!(score_white_cp(Score::Mate(2), Color::White), 9980.0);
        assert_eq!(score_white_cp(Score::Mate(-2), Color::White), -9980.0);
    }

    #[test]
    fn quality_thresholds_and_best_move() {
        let before = evaluated(30, "e2e4");
        let after = evaluated(-10, "e7e5");
        let (quality, loss) = classify("e2e4", Some(&before), Some(&after), Color::White);
        assert_eq!(quality, Quality::Best);
        assert_eq!(loss, Some(20));

        let after = evaluated(10, "e7e5");
        let (quality, loss) = classify("d2d4", Some(&before), Some(&after), Color::White);
        assert_eq!(quality, Quality::Good);
        assert_eq!(loss, Some(40));

        let after = evaluated(500, "e7e5");
        let (quality, loss) = classify("d2d4", Some(&before), Some(&after), Color::White);
        assert_eq!(quality, Quality::Blunder);
        assert!(loss.unwrap() > 250);
    }

    #[test]
    fn review_creates_one_position_per_ply_plus_root() {
        let games =
            crate::domain::parse_pgn(b"[Event \"Review test\"]\n\n1. e4 e5 2. Nf3 *\n").unwrap();
        let review = ReviewState::new(
            &games[0],
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            0,
        );
        assert_eq!(review.total(), 4);
        assert_eq!(review.progress(), (0, 4));
        assert!(review.moves(&games[0]).is_empty());
    }

    #[test]
    fn review_key_covers_game_engine_limit_and_book_prefix() {
        let games =
            crate::domain::parse_pgn(b"[Event \"Key test\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n").unwrap();
        let game = &games[0];
        let config = ReviewConfig {
            limit: SearchLimit::Depth(12),
        };
        let options = EngineOptions {
            threads: 2,
            hash_mb: 64,
        };
        let key = ReviewKey::new(game, config, 0, "/usr/bin/stockfish", options);

        assert_eq!(
            key,
            ReviewKey::new(game, config, 0, "/usr/bin/stockfish", options)
        );
        assert_ne!(
            key,
            ReviewKey::new(game, config, 2, "/usr/bin/stockfish", options)
        );
        assert_ne!(
            key,
            ReviewKey::new(
                game,
                ReviewConfig {
                    limit: SearchLimit::Depth(13)
                },
                0,
                "/usr/bin/stockfish",
                options
            )
        );
        assert_ne!(
            key,
            ReviewKey::new(game, config, 0, "/opt/stockfish", options)
        );
        assert_ne!(
            key,
            ReviewKey::new(
                game,
                config,
                0,
                "/usr/bin/stockfish",
                EngineOptions {
                    threads: 4,
                    hash_mb: 64
                }
            )
        );
        assert_ne!(
            key,
            ReviewKey::new(
                game,
                config,
                0,
                "/usr/bin/stockfish",
                EngineOptions {
                    threads: 2,
                    hash_mb: 128
                }
            )
        );

        let other = crate::domain::parse_pgn(b"[Event \"Other\"]\n\n1. d4 d5 *\n").unwrap();
        assert_ne!(
            key,
            ReviewKey::new(&other[0], config, 0, "/usr/bin/stockfish", options)
        );
        assert_eq!(
            ReviewKey::new(game, config, usize::MAX, "/usr/bin/stockfish", options),
            ReviewKey::new(
                game,
                config,
                game.mainline_len(),
                "/usr/bin/stockfish",
                options
            )
        );
    }

    #[test]
    fn review_skips_book_prefix_positions() {
        let games =
            crate::domain::parse_pgn(b"[Event \"Book skip\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n").unwrap();
        let game = &games[0];
        let review = ReviewState::new(
            game,
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            2,
        );
        // 4 plies total, 2 skipped -> position after 1...e5 plus 2.Nf3 and 2...Nc6 = 3 positions.
        assert_eq!(review.book_plies, 2);
        assert_eq!(review.total(), 3);

        let all_book = ReviewState::new(
            game,
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            4,
        );
        assert_eq!(all_book.total(), 0);
        assert!(!all_book.running);
        assert!(all_book.complete());
    }

    #[test]
    fn grades_completed_position_pairs() {
        let games = crate::domain::parse_pgn(b"[Event \"Grade test\"]\n\n1. e4 e5 *\n").unwrap();
        let game = &games[0];
        let mut review = ReviewState::new(
            game,
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            0,
        );
        review.evaluated[0] = Some(Evaluated {
            eval: Some(Score::Cp(20)),
            best: Some("e2e4".to_string()),
            white_cp: Some(20.0),
        });
        review.evaluated[1] = Some(Evaluated {
            eval: Some(Score::Cp(-10)),
            best: Some("e7e5".to_string()),
            white_cp: Some(10.0),
        });
        review.evaluated[2] = Some(Evaluated {
            eval: Some(Score::Cp(30)),
            best: Some("g1f3".to_string()),
            white_cp: Some(30.0),
        });
        let moves = review.moves(game);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].quality, Quality::Best);
        assert_eq!(moves[0].cp_loss, Some(10));
        assert_eq!(moves[1].quality, Quality::Best);
        assert_eq!(moves[1].cp_loss, Some(20));
        assert!(accuracy(&moves).unwrap() > 90.0);
        let summary = accuracy_summary(&moves);
        assert!(summary.white.unwrap() > 90.0);
        assert!(summary.black.unwrap() > 90.0);
        assert_eq!(summary.white_acpl, Some(10.0));
        assert_eq!(summary.black_acpl, Some(20.0));
    }

    #[test]
    fn accuracy_uses_known_losses_only() {
        let make = |loss| MoveReview {
            node: 1,
            number: 1,
            color: Color::White,
            san: "e4".to_string(),
            played: "e2e4".to_string(),
            eval_before: Some(Score::Cp(0)),
            eval_after: Some(Score::Cp(0)),
            best: None,
            cp_loss: Some(loss),
            win_before: None,
            win_after: None,
            accuracy: None,
            quality: Quality::Good,
        };
        let accuracy = accuracy(&[make(0), make(400)]).unwrap();
        assert!((accuracy - (100.0 + 100.0 * (-1.0f64).exp()) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn accuracy_summary_caps_acpl_and_penalizes_blunders_with_harmonic_mean() {
        let good = MoveReview {
            node: 1,
            number: 1,
            color: Color::White,
            san: "e4".to_string(),
            played: "e2e4".to_string(),
            eval_before: Some(Score::Cp(20)),
            eval_after: Some(Score::Cp(-20)),
            best: Some("e2e4".to_string()),
            cp_loss: Some(0),
            win_before: Some(52.0),
            win_after: Some(52.0),
            accuracy: Some(100.0),
            quality: Quality::Best,
        };
        let blunder = MoveReview {
            node: 3,
            number: 2,
            color: Color::White,
            san: "Qh5".to_string(),
            played: "d1h5".to_string(),
            eval_before: Some(Score::Cp(20)),
            eval_after: Some(Score::Cp(1500)),
            best: Some("g1f3".to_string()),
            cp_loss: Some(1520),
            win_before: Some(52.0),
            win_after: Some(2.0),
            accuracy: Some(eval::move_accuracy(52.0, 2.0)),
            quality: Quality::Blunder,
        };
        let forced = MoveReview {
            node: 2,
            number: 1,
            color: Color::Black,
            san: "Kh8".to_string(),
            played: "g8h8".to_string(),
            eval_before: Some(Score::Cp(0)),
            eval_after: Some(Score::Cp(0)),
            best: None,
            cp_loss: None,
            win_before: None,
            win_after: None,
            accuracy: None,
            quality: Quality::Forced,
        };

        let summary = accuracy_summary(&[good, forced, blunder]);
        assert_eq!(summary.white_moves, 2);
        assert_eq!(summary.black_moves, 0);
        assert!(summary.black.is_none());
        // 0 and 1520 (capped to 1000) -> average 500.0
        assert_eq!(summary.white_acpl, Some(500.0));
        // Arithmetic mean would be > 55%, harmonic mean pulls the combined accuracy down.
        assert!(summary.white.unwrap() < 50.0);
    }
}
