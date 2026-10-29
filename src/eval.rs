//! Position evaluation normalization, win-probability conversion, and
//! Lichess-style per-move accuracy math.

use crate::chess::Color;
use crate::domain::PgnEval;
use crate::engine::Score;

/// Origin of a board evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalSource {
    Local,
    Cloud,
    Pgn,
}

impl EvalSource {
    fn tie_rank(self) -> u8 {
        match self {
            Self::Local => 3,
            Self::Cloud => 2,
            Self::Pgn => 1,
        }
    }
}

/// Evaluation normalized to White's perspective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteScore {
    /// Centipawns (`+100` = +1.00 for White).
    Cp(i32),
    /// Moves to mate (`+3` = White mates in 3, `-3` = Black mates in 3).
    Mate(i32),
}

impl WhiteScore {
    /// Convert a side-to-move engine [`Score`] into White's perspective.
    pub fn from_side_to_move(score: Score, turn: Color) -> Self {
        let sign = match turn {
            Color::White => 1,
            Color::Black => -1,
        };
        match score {
            Score::Cp(cp) => Self::Cp(cp.saturating_mul(sign)),
            Score::Mate(mate) => Self::Mate(mate.saturating_mul(sign)),
        }
    }

    /// Win probability for White in `[0.0, 100.0]` using the Lichess logistic curve.
    pub fn win_percent(self) -> f64 {
        match self {
            Self::Cp(cp) => {
                let clamped = cp.clamp(-1000, 1000) as f64;
                let multiplier = -0.00368208;
                50.0 + 50.0 * (2.0 / (1.0 + (multiplier * clamped).exp()) - 1.0)
            }
            Self::Mate(mate) => {
                if mate >= 0 {
                    100.0
                } else {
                    0.0
                }
            }
        }
    }

    /// Win probability for `color` in `[0.0, 100.0]`.
    pub fn win_percent_for(self, color: Color) -> f64 {
        let white = self.win_percent();
        match color {
            Color::White => white,
            Color::Black => 100.0 - white,
        }
    }

    /// Compact score label (`+0.35`, `-1.20`, `0.00`, `#3`, `#-3`).
    pub fn label(self) -> String {
        match self {
            Self::Cp(cp) if cp > 0 => format!("+{:.2}", cp as f64 / 100.0),
            Self::Cp(cp) if cp < 0 => format!("{:.2}", cp as f64 / 100.0),
            Self::Cp(_) => "0.00".to_string(),
            Self::Mate(mate) => format!("#{mate}"),
        }
    }
}

/// Evaluation displayed on the board bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardEval {
    pub score: WhiteScore,
    pub depth: u32,
    pub source: EvalSource,
}

impl BoardEval {
    pub fn from_local(score: Score, turn: Color, depth: u32) -> Self {
        Self {
            score: WhiteScore::from_side_to_move(score, turn),
            depth,
            source: EvalSource::Local,
        }
    }

    pub fn from_cloud(cp: Option<i32>, mate: Option<i32>, depth: u32) -> Option<Self> {
        let score = if let Some(mate) = mate {
            WhiteScore::Mate(mate)
        } else {
            WhiteScore::Cp(cp?)
        };
        Some(Self {
            score,
            depth,
            source: EvalSource::Cloud,
        })
    }

    pub fn from_pgn(eval: PgnEval) -> Option<Self> {
        let score = if let Some(mate) = eval.mate {
            WhiteScore::Mate(mate)
        } else {
            WhiteScore::Cp(eval.cp?)
        };
        Some(Self {
            score,
            depth: 0,
            source: EvalSource::Pgn,
        })
    }

    /// Human-readable hover tooltip (`White +0.35 · Lichess cloud d24`).
    pub fn tooltip(self) -> String {
        let side = match self.score {
            WhiteScore::Cp(cp) if cp > 0 => format!("White +{:.2}", cp as f64 / 100.0),
            WhiteScore::Cp(cp) if cp < 0 => format!("Black +{:.2}", (-cp) as f64 / 100.0),
            WhiteScore::Cp(_) => "Equal (0.00)".to_string(),
            WhiteScore::Mate(mate) if mate > 0 => format!("White mates in {mate}"),
            WhiteScore::Mate(mate) => format!("Black mates in {}", -mate),
        };
        let source = match (self.source, self.depth) {
            (EvalSource::Local, d) if d > 0 => format!("local engine d{d}"),
            (EvalSource::Local, _) => "local engine".to_string(),
            (EvalSource::Cloud, d) if d > 0 => format!("Lichess cloud d{d}"),
            (EvalSource::Cloud, _) => "Lichess cloud".to_string(),
            (EvalSource::Pgn, _) => "PGN eval".to_string(),
        };
        format!("{side} · {source}")
    }
}

/// Select the most authoritative evaluation: highest depth wins; ties prefer
/// `Local > Cloud > Pgn`.
pub fn choose(
    local: Option<BoardEval>,
    cloud: Option<BoardEval>,
    pgn: Option<BoardEval>,
) -> Option<BoardEval> {
    [local, cloud, pgn]
        .into_iter()
        .flatten()
        .max_by_key(|eval| (eval.depth, eval.source.tie_rank()))
}

/// Lichess per-move accuracy in `[0.0, 100.0]` from the mover's win percentages
/// before and after the move.
pub fn move_accuracy(win_before: f64, win_after: f64) -> f64 {
    if win_after >= win_before {
        return 100.0;
    }
    let diff = win_before - win_after;
    let raw = 103.1668 * (-0.04354 * diff).exp() - 3.1669 + 1.0;
    raw.clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_local_score_from_side_to_move_to_white() {
        let white_turn = BoardEval::from_local(Score::Cp(45), Color::White, 18);
        assert_eq!(white_turn.score, WhiteScore::Cp(45));

        let black_turn = BoardEval::from_local(Score::Cp(45), Color::Black, 18);
        assert_eq!(black_turn.score, WhiteScore::Cp(-45));

        let black_mate = BoardEval::from_local(Score::Mate(3), Color::Black, 22);
        assert_eq!(black_mate.score, WhiteScore::Mate(-3));
    }

    #[test]
    fn chooses_highest_depth_and_breaks_ties_local_then_cloud_then_pgn() {
        let local = BoardEval::from_local(Score::Cp(20), Color::White, 18);
        let cloud_deeper = BoardEval::from_cloud(Some(35), None, 26).unwrap();
        let pgn = BoardEval::from_pgn(PgnEval {
            cp: Some(15),
            mate: None,
        })
        .unwrap();

        assert_eq!(
            choose(Some(local), Some(cloud_deeper), Some(pgn)),
            Some(cloud_deeper)
        );

        let cloud_same = BoardEval::from_cloud(Some(35), None, 18).unwrap();
        assert_eq!(
            choose(Some(local), Some(cloud_same), Some(pgn)),
            Some(local)
        );
        assert_eq!(choose(None, None, Some(pgn)), Some(pgn));
        assert_eq!(choose(None, None, None), None);
    }

    #[test]
    fn computes_win_percentages_and_clamps_extremes() {
        let eq = WhiteScore::Cp(0).win_percent();
        assert!((eq - 50.0).abs() < 1e-6);

        let plus_one = WhiteScore::Cp(100).win_percent();
        let minus_one = WhiteScore::Cp(-100).win_percent();
        assert!(plus_one > 58.0 && plus_one < 60.0);
        assert!((plus_one + minus_one - 100.0).abs() < 1e-6);

        assert_eq!(
            WhiteScore::Cp(1000).win_percent(),
            WhiteScore::Cp(2500).win_percent()
        );
        assert_eq!(
            WhiteScore::Cp(-1000).win_percent(),
            WhiteScore::Cp(-2500).win_percent()
        );
        assert_eq!(WhiteScore::Mate(2).win_percent(), 100.0);
        assert_eq!(WhiteScore::Mate(-2).win_percent(), 0.0);
    }

    #[test]
    fn move_accuracy_rewards_equal_or_better_and_penalizes_drops() {
        assert_eq!(move_accuracy(50.0, 50.0), 100.0);
        assert_eq!(move_accuracy(50.0, 55.0), 100.0);
        let slight_loss = move_accuracy(55.0, 50.0);
        let blunder_loss = move_accuracy(65.0, 20.0);
        assert!(slight_loss > 80.0 && slight_loss < 100.0);
        assert!(blunder_loss < 30.0);
    }

    #[test]
    fn formats_labels_and_tooltips() {
        assert_eq!(WhiteScore::Cp(35).label(), "+0.35");
        assert_eq!(WhiteScore::Cp(-120).label(), "-1.20");
        assert_eq!(WhiteScore::Cp(0).label(), "0.00");
        assert_eq!(WhiteScore::Mate(3).label(), "#3");
        assert_eq!(WhiteScore::Mate(-3).label(), "#-3");

        let cloud = BoardEval::from_cloud(Some(35), None, 24).unwrap();
        assert_eq!(cloud.tooltip(), "White +0.35 · Lichess cloud d24");

        let mate = BoardEval::from_local(Score::Mate(2), Color::Black, 16);
        assert_eq!(mate.tooltip(), "Black mates in 2 · local engine d16");
    }
}
