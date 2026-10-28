//! Sequential game review on top of the UCI engine.
//!
//! The review analyzes every mainline position once. A move is graded by
//! comparing the engine's evaluation before the move with the evaluation after
//! the played move, both from the mover's point of view. This is deliberately
//! a display-oriented, engine-relative grade; it is not a claim of objective
//! accuracy.

use std::time::Instant;

use crate::chess::{Color, EnPassantMode, Fen, Position};

use crate::domain::Game;
use crate::engine::{EngineHandle, EngineInfo, Score, SearchLimit};

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
    pub quality: Quality,
}

/// In-progress or completed review of a game.
pub struct ReviewState {
    pub config: ReviewConfig,
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
    pub fn new(game: &Game, config: ReviewConfig) -> Self {
        let mut nodes = vec![0usize];
        nodes.extend(game.mainline());
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
            config,
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

    pub fn total(&self) -> usize {
        self.positions.len()
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.positions.len())
    }

    pub fn complete(&self) -> bool {
        !self.running && self.next >= self.positions.len()
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
            let (quality, cp_loss) = if legal == 1 {
                (Quality::Forced, None)
            } else {
                classify(&ply.uci, Some(before), Some(after), ply.color)
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

/// Average per-move heuristic accuracy. This is intentionally simple and is
/// labelled as an estimate in the UI.
pub fn accuracy(moves: &[MoveReview]) -> Option<f64> {
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
        );
        assert_eq!(review.total(), 4);
        assert_eq!(review.progress(), (0, 4));
        assert!(review.moves(&games[0]).is_empty());
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
            quality: Quality::Good,
        };
        let accuracy = accuracy(&[make(0), make(400)]).unwrap();
        assert!((accuracy - (100.0 + 100.0 * (-1.0f64).exp()) / 2.0).abs() < 1e-9);
    }
}
