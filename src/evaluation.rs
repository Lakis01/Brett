//! Held-out metrics for the preparation predictor.
//!
//! The evaluator scores a predictive distribution against the move that was
//! actually played. It supports the residual mass used by the predictor, so a
//! move that never appeared in training is scored as one of the unobserved
//! legal moves rather than as an impossible event.

use crate::players::{MovePrediction, Prediction};

const EPSILON: f64 = 1e-12;

/// Aggregate metrics over a set of held-out moves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub cases: usize,
    pub log_loss: f64,
    pub brier: f64,
    pub top1: f64,
    pub top3: f64,
}

impl Metrics {
    fn finish(&mut self) {
        if self.cases == 0 {
            return;
        }
        let n = self.cases as f64;
        self.log_loss /= n;
        self.brier /= n;
        self.top1 /= n;
        self.top3 /= n;
    }
}

/// Score for one held-out position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaseScore {
    pub log_loss: f64,
    pub brier: f64,
    pub top1: bool,
    pub top3: bool,
    pub top_probability: f64,
}

/// Score one prediction against the move that was actually played.
pub fn score_prediction(prediction: &Prediction, actual: u32) -> CaseScore {
    let actual_move = prediction.moves.iter().find(|mv| mv.mv == actual);
    let actual_seen = actual_move.is_some();
    let probability = actual_move.map_or_else(
        || {
            if prediction.unseen_moves > 0 {
                prediction.unseen_probability / prediction.unseen_moves as f64
            } else {
                EPSILON
            }
        },
        |mv| mv.probability,
    );

    let mut brier = 0.0;
    for mv in &prediction.moves {
        let target = if mv.mv == actual { 1.0 } else { 0.0 };
        brier += (mv.probability - target).powi(2);
    }
    // The residual is spread uniformly over the unobserved legal moves. For
    // Brier score that matters: a no-data prediction is uniform, not certain.
    let unseen_moves = prediction.unseen_moves as f64;
    if unseen_moves > 0.0 {
        let unseen_each = prediction.unseen_probability / unseen_moves;
        if actual_seen {
            brier += unseen_moves * unseen_each.powi(2);
        } else {
            brier += (unseen_each - 1.0).powi(2) + (unseen_moves - 1.0) * unseen_each.powi(2);
        }
    } else if !actual_seen {
        brier += 1.0;
    }

    CaseScore {
        log_loss: -probability.max(EPSILON).ln(),
        brier,
        top1: prediction.moves.first().is_some_and(|mv| mv.mv == actual),
        top3: prediction.moves.iter().take(3).any(|mv| mv.mv == actual),
        top_probability: prediction.moves.first().map_or(0.0, |mv| mv.probability),
    }
}

/// Accumulates metrics and the top-1 calibration observations.
#[derive(Clone, Debug, Default)]
pub struct Evaluator {
    metrics: Metrics,
    calibration: Vec<(f64, bool)>,
}

impl Evaluator {
    pub fn record(&mut self, prediction: &Prediction, actual: u32) {
        let score = score_prediction(prediction, actual);
        self.metrics.cases += 1;
        self.metrics.log_loss += score.log_loss;
        self.metrics.brier += score.brier;
        self.metrics.top1 += f64::from(score.top1);
        self.metrics.top3 += f64::from(score.top3);
        self.calibration.push((score.top_probability, score.top1));
    }

    pub fn metrics(&self) -> Metrics {
        let mut metrics = self.metrics;
        metrics.finish();
        metrics
    }

    pub fn calibration(&self, bins: usize) -> Vec<CalibrationBin> {
        calibration_bins(&self.calibration, bins)
    }
}

/// One bin of a top-1 reliability curve.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CalibrationBin {
    pub lower: f64,
    pub upper: f64,
    pub cases: usize,
    pub mean_predicted: f64,
    pub observed: f64,
}

fn calibration_bins(cases: &[(f64, bool)], bins: usize) -> Vec<CalibrationBin> {
    let bins = bins.max(1);
    let mut sums = vec![(0usize, 0.0, 0usize); bins];
    for &(probability, correct) in cases {
        let index = ((probability.clamp(0.0, 1.0) * bins as f64) as usize).min(bins - 1);
        sums[index].0 += 1;
        sums[index].1 += probability;
        sums[index].2 += usize::from(correct);
    }
    sums.into_iter()
        .enumerate()
        .filter(|(_, (count, _, _))| *count > 0)
        .map(
            |(index, (count, probability_sum, correct))| CalibrationBin {
                lower: index as f64 / bins as f64,
                upper: (index + 1) as f64 / bins as f64,
                cases: count,
                mean_predicted: probability_sum / count as f64,
                observed: correct as f64 / count as f64,
            },
        )
        .collect()
}

/// Convenience wrapper for callers that have slices of `MovePrediction`.
pub fn move_probabilities(moves: &[MovePrediction]) -> Vec<(u32, f64)> {
    moves.iter().map(|mv| (mv.mv, mv.probability)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::{MovePrediction, Prediction, PriorSource};

    fn prediction(moves: &[(u32, f64)], unseen: f64, unseen_moves: usize) -> Prediction {
        Prediction {
            moves: moves
                .iter()
                .map(|&(mv, probability)| MovePrediction {
                    mv,
                    probability,
                    probability_low: probability,
                    probability_high: probability,
                    ..MovePrediction::default()
                })
                .collect(),
            unseen_probability: unseen,
            unseen_moves,
            legal_moves: moves.len() + unseen_moves,
            prior_source: PriorSource::Reference,
            ..Prediction::default()
        }
    }

    #[test]
    fn scores_observed_and_unseen_moves() {
        let observed = prediction(&[(1, 0.7), (2, 0.2)], 0.1, 1);
        let score = score_prediction(&observed, 1);
        assert!((score.log_loss + 0.7f64.ln()).abs() < 1e-9);
        assert!((score.brier - (0.3f64.powi(2) + 0.2f64.powi(2) + 0.1f64.powi(2))).abs() < 1e-9);
        assert!(score.top1);
        assert!(score.top3);

        let unseen = score_prediction(&observed, 9);
        assert!((unseen.log_loss + 0.1f64.ln()).abs() < 1e-9);
        assert!(!unseen.top1);
        assert!(!unseen.top3);

        let no_data = prediction(&[], 1.0, 20);
        let uniform = score_prediction(&no_data, 9);
        assert!((uniform.log_loss - 20f64.ln()).abs() < 1e-9);
        assert!((uniform.brier - 0.95).abs() < 1e-9);
    }

    #[test]
    fn aggregate_metrics_are_averages() {
        let prediction = prediction(&[(1, 0.75), (2, 0.25)], 0.0, 0);
        let mut evaluator = Evaluator::default();
        evaluator.record(&prediction, 1);
        evaluator.record(&prediction, 2);
        let metrics = evaluator.metrics();
        assert_eq!(metrics.cases, 2);
        assert!((metrics.log_loss - (-(0.75f64.ln() + 0.25f64.ln()) / 2.0)).abs() < 1e-9);
        let expected_brier = (0.25f64.powi(2) * 2.0 + 0.75f64.powi(2) * 2.0) / 2.0;
        assert!((metrics.brier - expected_brier).abs() < 1e-9);
        assert!((metrics.top1 - 0.5).abs() < 1e-9);
        assert!((metrics.top3 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn calibration_bins_use_top_prediction() {
        let mut evaluator = Evaluator::default();
        let prediction = prediction(&[(1, 0.9), (2, 0.1)], 0.0, 0);
        evaluator.record(&prediction, 1);
        evaluator.record(&prediction, 2);
        let bins = evaluator.calibration(10);
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].cases, 2);
        assert!((bins[0].mean_predicted - 0.9).abs() < 1e-9);
        assert!((bins[0].observed - 0.5).abs() < 1e-9);
    }
}
