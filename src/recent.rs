//! In-memory opening-tree and move-trend statistics for position indexes and
//! recently downloaded games.
//!
//! Trends compare a rolling recent window (last 6, 12, or 24 months) against a
//! longer-term baseline (`AllPrior` or `Prior24Months`) using the quarterly
//! date buckets stored in [`crate::position::MoveStat`] (and in [`RecentTree`]).
//! Small samples are shrunk toward the baseline via empirical-Bayes
//! (Beta-Binomial) smoothing so a 2-game spike is never reported as a trend.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::chess::UciMove;
use crate::domain::Game;
use crate::players;
use crate::position::{
    self, MoveStat, PositionStats, TREND_BUCKETS, date_to_quarter, quarter_label,
};

/// Rolling recent window used for move-popularity trends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrendWindow {
    /// Last 6 months (2 quarters).
    SixMonths,
    /// Last 12 months (4 quarters).
    #[default]
    TwelveMonths,
    /// Last 24 months (8 quarters).
    TwentyFourMonths,
}

impl TrendWindow {
    pub const ALL: [Self; 3] = [Self::SixMonths, Self::TwelveMonths, Self::TwentyFourMonths];

    pub fn quarters(self) -> usize {
        match self {
            Self::SixMonths => 2,
            Self::TwelveMonths => 4,
            Self::TwentyFourMonths => 8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SixMonths => "6 months",
            Self::TwelveMonths => "12 months",
            Self::TwentyFourMonths => "24 months",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::SixMonths => "6m",
            Self::TwelveMonths => "12m",
            Self::TwentyFourMonths => "24m",
        }
    }

    pub fn config_value(self) -> &'static str {
        self.short_label()
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "6m" | "6" | "six_months" => Self::SixMonths,
            "24m" | "24" | "twenty_four_months" => Self::TwentyFourMonths,
            _ => Self::TwelveMonths,
        }
    }
}

/// Historical baseline period against which the recent window is compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrendBaseline {
    /// All dated games prior to the recent window.
    #[default]
    AllPrior,
    /// The 24 months (8 quarters) immediately preceding the recent window.
    Prior24Months,
}

impl TrendBaseline {
    pub const ALL: [Self; 2] = [Self::AllPrior, Self::Prior24Months];

    pub fn label(self) -> &'static str {
        match self {
            Self::AllPrior => "All prior games",
            Self::Prior24Months => "Prior 24 months",
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::AllPrior => "all_prior",
            Self::Prior24Months => "prior_24m",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "prior_24m" | "24m" | "prior24m" => Self::Prior24Months,
            _ => Self::AllPrior,
        }
    }
}

/// Configuration for computing move-popularity trends at a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrendConfig {
    pub window: TrendWindow,
    pub baseline: TrendBaseline,
    /// Minimum dated games required in both the recent window and the baseline
    /// at the position before any trend badge is shown.
    pub min_recent_games: u32,
}

impl Default for TrendConfig {
    fn default() -> Self {
        Self {
            window: TrendWindow::TwelveMonths,
            baseline: TrendBaseline::AllPrior,
            min_recent_games: 10,
        }
    }
}

/// Classification of a move's recent popularity relative to its baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrendBadge {
    /// Genuinely new idea: near-zero historical share, meaningful recent share.
    New,
    /// Revival: played historically, quiet or lower in between, rising strongly now.
    Revival,
    /// Rising share relative to a meaningful historical baseline.
    Rising,
    /// Stable share within normal sampling variation.
    Steady,
    /// Declining share relative to the historical baseline.
    Falling,
}

impl TrendBadge {
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Revival => "revival",
            Self::Rising => "rising",
            Self::Steady => "steady",
            Self::Falling => "falling",
        }
    }
}

/// Statistical confidence level of the trend estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrendConfidence {
    Low,
    Medium,
    High,
}

impl TrendConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// One quarterly bucket in a move's chronological popularity series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrendBucket {
    pub quarter: u32,
    pub move_games: u32,
    pub position_games: u32,
    /// Share of games at the position in this quarter (`0.0..=1.0`), or `None`
    /// when `position_games == 0`.
    pub share: Option<f64>,
}

/// Rolling-window trend summary and sparkline series for one move at a position.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveTrend {
    pub mv: u32,
    pub recent_games: u32,
    pub recent_total: u32,
    /// Raw share in the recent window (`recent_games / recent_total`).
    pub recent_share: Option<f64>,
    /// Empirical-Bayes shrunk recent share pulled toward `baseline_share`.
    pub shrunk_recent_share: Option<f64>,
    pub baseline_games: u32,
    pub baseline_total: u32,
    pub baseline_share: Option<f64>,
    /// Raw percentage-point difference `(recent_share - baseline_share) * 100`.
    pub raw_delta_pp: Option<f64>,
    /// Shrunk percentage-point difference `(shrunk_recent_share - baseline_share) * 100`.
    pub shrunk_delta_pp: Option<f64>,
    /// Badge when both windows meet the minimum sample threshold (`None` when
    /// history is sparse).
    pub badge: Option<TrendBadge>,
    pub confidence: TrendConfidence,
    /// Chronological quarterly buckets (8 to 16 quarters ending at the
    /// position's latest dated game) for sparkline rendering and hover inspection.
    pub buckets: Vec<TrendBucket>,
}

impl MoveTrend {
    /// Normalized sparkline points `(x, y)` with both axes in `0.0..=1.0`.
    ///
    /// Only quarters with dated games (`position_games > 0` and a share) emit
    /// points; `x` is the chronological bucket index over the full bucket
    /// slice and `y` is the share linearly normalized into the data range
    /// (higher share maps to higher `y`). Returns empty when there is no
    /// badge or fewer than two active quarters.
    pub fn sparkline_points(&self) -> Vec<(f32, f32)> {
        let Some((min_s, span)) = self.sparkline_scale() else {
            return Vec::new();
        };
        let denom = self.buckets.len().saturating_sub(1).max(1) as f32;
        self.buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.position_games > 0 && b.share.is_some())
            .map(|(idx, b)| {
                let share = b.share.unwrap_or(0.0);
                let y = ((share - min_s) / span).clamp(0.0, 1.0) as f32;
                (idx as f32 / denom, y)
            })
            .collect()
    }

    /// Baseline share mapped into the same normalized `0.0..=1.0` scale as
    /// [`Self::sparkline_points`], or `None` when the sparkline is empty or
    /// there is no baseline share.
    pub fn sparkline_baseline_y(&self) -> Option<f32> {
        let baseline = self.baseline_share?;
        let (min_s, span) = self.sparkline_scale()?;
        Some(((baseline - min_s) / span).clamp(0.0, 1.0) as f32)
    }

    /// Shared `(min, span)` normalization over active shares plus the baseline.
    /// Enforces a minimum 0.20 span centered on the mid, clamped to `0..=1`.
    fn sparkline_scale(&self) -> Option<(f64, f64)> {
        self.badge?;
        let active: Vec<(usize, f64)> = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.position_games > 0 && b.share.is_some())
            .map(|(idx, b)| (idx, b.share.unwrap_or(0.0)))
            .collect();
        if active.len() < 2 {
            return None;
        }
        let mut min_s = active.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
        let mut max_s = active
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::NEG_INFINITY, f64::max);
        if let Some(base) = self.baseline_share {
            min_s = min_s.min(base);
            max_s = max_s.max(base);
        }
        if max_s - min_s < 0.20 {
            let mid = (min_s + max_s) / 2.0;
            let mut lo = mid - 0.10;
            let mut hi = mid + 0.10;
            if lo < 0.0 {
                hi -= lo;
                lo = 0.0;
            }
            if hi > 1.0 {
                lo -= hi - 1.0;
                hi = 1.0;
            }
            min_s = lo.clamp(0.0, 1.0);
            max_s = hi.clamp(0.0, 1.0);
        }
        min_s = min_s.clamp(0.0, 1.0);
        max_s = max_s.clamp(0.0, 1.0);
        let span = (max_s - min_s).max(1e-6);
        Some((min_s, span))
    }

    /// Compact badge + delta label for the Tree table (for example
    /// `"new +32pp"` or `"rising +14pp"`), or `None` when data is too sparse.
    pub fn compact_label(&self) -> Option<String> {
        let badge = self.badge?;
        let delta = self.shrunk_delta_pp.unwrap_or(0.0);
        match badge {
            TrendBadge::New => Some(format!("new {:+.0}pp", delta)),
            TrendBadge::Revival => Some(format!("revival {:+.0}pp", delta)),
            TrendBadge::Rising => Some(format!("rising {:+.0}pp", delta)),
            TrendBadge::Falling => Some(format!("falling {:+.0}pp", delta)),
            TrendBadge::Steady => Some(format!("steady {:+.0}pp", delta)),
        }
    }

    /// Detailed multi-line hover explanation including sample counts, shrunk vs
    /// raw delta, confidence, and per-quarter bucket shares.
    pub fn tooltip(&self, config: TrendConfig) -> String {
        let mut lines = Vec::new();
        if let Some(badge) = self.badge {
            lines.push(format!(
                "Trend: {} ({} confidence)",
                badge.label(),
                self.confidence.label()
            ));
        } else {
            lines.push(format!(
                "Insufficient dated history for a trend badge (need ≥ {} recent and ≥ {} baseline games at this position).",
                config.min_recent_games, config.min_recent_games
            ));
        }
        let recent_pct = self
            .recent_share
            .map(|s| format!("{:.1}%", s * 100.0))
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!(
            "Recent ({}): {}/{} games ({recent_pct})",
            config.window.label(),
            self.recent_games,
            self.recent_total
        ));
        let base_pct = self
            .baseline_share
            .map(|s| format!("{:.1}%", s * 100.0))
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!(
            "Baseline ({}): {}/{} games ({base_pct})",
            config.baseline.label(),
            self.baseline_games,
            self.baseline_total
        ));
        if let (Some(shrunk), Some(raw)) = (self.shrunk_delta_pp, self.raw_delta_pp) {
            lines.push(format!(
                "Delta: {:+.1} pp shrunk ({:+.1} pp raw)",
                shrunk, raw
            ));
        }
        let non_empty: Vec<String> = self
            .buckets
            .iter()
            .filter(|b| b.position_games > 0)
            .map(|b| {
                let pct = b.share.unwrap_or(0.0) * 100.0;
                format!(
                    "  {}: {:>4.0}% ({}/{})",
                    quarter_label(b.quarter),
                    pct,
                    b.move_games,
                    b.position_games
                )
            })
            .collect();
        if !non_empty.is_empty() {
            lines.push("Quarterly share:".to_string());
            lines.extend(non_empty);
        }
        lines.join("\n")
    }
}

/// Compute rolling-window popularity trends for all moves at a position.
///
/// `moves` should come from [`crate::position::PositionIndex::moves`] or
/// [`RecentTree::moves`]. Even if caller-constructed `MoveStat`s have
/// different `anchor_quarter`s, this function aligns them to the position's
/// latest quarter before aggregating.
pub fn position_trends(moves: &[MoveStat], config: TrendConfig) -> Vec<MoveTrend> {
    if moves.is_empty() {
        return Vec::new();
    }
    let pos_anchor = moves.iter().map(|m| m.anchor_quarter).max().unwrap_or(0);
    if pos_anchor == 0 {
        return moves
            .iter()
            .map(|m| MoveTrend {
                mv: m.mv,
                recent_games: 0,
                recent_total: 0,
                recent_share: None,
                shrunk_recent_share: None,
                baseline_games: 0,
                baseline_total: 0,
                baseline_share: None,
                raw_delta_pp: None,
                shrunk_delta_pp: None,
                badge: None,
                confidence: TrendConfidence::Low,
                buckets: Vec::new(),
            })
            .collect();
    }

    let aligned: Vec<MoveStat> = moves
        .iter()
        .copied()
        .map(|mut m| {
            align_move_stat(&mut m, pos_anchor);
            m
        })
        .collect();

    let mut pos_buckets = [0u32; TREND_BUCKETS];
    let mut pos_older_dated = 0u32;
    for m in &aligned {
        pos_older_dated = pos_older_dated.saturating_add(m.older_dated);
        for (dst, &src) in pos_buckets.iter_mut().zip(m.buckets.iter()) {
            *dst = dst.saturating_add(src);
        }
    }

    let w = config.window.quarters().min(TREND_BUCKETS);
    let recent_start = TREND_BUCKETS - w;
    let recent_total: u32 = pos_buckets[recent_start..].iter().sum();

    let (base_slice_start, include_older) = match config.baseline {
        TrendBaseline::AllPrior => (0, true),
        TrendBaseline::Prior24Months => (recent_start.saturating_sub(8), false),
    };
    let baseline_total: u32 = pos_buckets[base_slice_start..recent_start]
        .iter()
        .sum::<u32>()
        .saturating_add(if include_older { pos_older_dated } else { 0 });

    // Include at least the last 8 quarters in the sparkline, or earlier if
    // any quarter in `0..8` has dated games.
    let first_non_empty = pos_buckets
        .iter()
        .position(|&c| c > 0)
        .unwrap_or(TREND_BUCKETS.saturating_sub(8));
    let spark_start = first_non_empty.min(TREND_BUCKETS.saturating_sub(8));

    let min_pos_games = config.min_recent_games.max(1);
    let min_move_games = (min_pos_games / 3).max(3);
    let kappa = (min_pos_games as f64).max(10.0);

    aligned
        .into_iter()
        .map(|m| {
            let recent_games: u32 = m.buckets[recent_start..].iter().sum();
            let baseline_games: u32 = m.buckets[base_slice_start..recent_start]
                .iter()
                .sum::<u32>()
                .saturating_add(if include_older { m.older_dated } else { 0 });
            let older_all_time: u32 = m
                .older_dated
                .saturating_add(m.buckets[..recent_start].iter().sum::<u32>());

            let recent_share =
                (recent_total > 0).then(|| f64::from(recent_games) / f64::from(recent_total));
            let baseline_share =
                (baseline_total > 0).then(|| f64::from(baseline_games) / f64::from(baseline_total));

            let shrunk_recent_share = match (recent_share, baseline_share) {
                (Some(_), Some(p0)) => {
                    Some((f64::from(recent_games) + kappa * p0) / (f64::from(recent_total) + kappa))
                }
                (Some(r), None) => Some(r),
                _ => None,
            };

            let raw_delta_pp = recent_share
                .zip(baseline_share)
                .map(|(r, b)| (r - b) * 100.0);
            let shrunk_delta_pp = shrunk_recent_share
                .zip(baseline_share)
                .map(|(r, b)| (r - b) * 100.0);

            let sufficient_sample =
                recent_total >= min_pos_games && baseline_total >= min_pos_games;

            let (badge, confidence) = if !sufficient_sample {
                (None, TrendConfidence::Low)
            } else {
                let p_shrunk = shrunk_recent_share.unwrap_or(0.0);
                let p_raw = recent_share.unwrap_or(0.0);
                let p0 = baseline_share.unwrap_or(0.0);
                let p0_floor = p0.max(1.0 / (f64::from(baseline_total) + 2.0));
                let var_recent =
                    (p_shrunk * (1.0 - p_shrunk)).max(0.01) / (f64::from(recent_total) + kappa);
                let var_base = (p0_floor * (1.0 - p0_floor)).max(0.01) / f64::from(baseline_total);
                let se = (var_recent + var_base).sqrt().max(1e-6);
                let z = (p_shrunk - p0) / se;
                let delta_shrunk = shrunk_delta_pp.unwrap_or(0.0);

                let conf = if recent_total >= 30 && baseline_total >= 40 && z.abs() >= 2.0 {
                    TrendConfidence::High
                } else if z.abs() >= 1.25 {
                    TrendConfidence::Medium
                } else {
                    TrendConfidence::Low
                };

                // Check for a gap (dormant period) right before the recent window
                // after earlier historical play, which signals a revival.
                let quiet_window_start = recent_start.saturating_sub(4);
                let quiet_before_recent: u32 =
                    m.buckets[quiet_window_start..recent_start].iter().sum();
                let earlier_history: u32 = m
                    .older_dated
                    .saturating_add(m.buckets[..quiet_window_start].iter().sum::<u32>());

                let badge = if recent_games >= min_move_games && delta_shrunk >= 5.0 && z >= 1.2 {
                    if p0 < 0.02 && older_all_time <= 1 && p_raw >= 0.12 {
                        Some(TrendBadge::New)
                    } else if (earlier_history >= 2 && quiet_before_recent == 0 && p_raw >= 0.12)
                        || (p0 >= 0.03
                            && earlier_history >= 2
                            && delta_shrunk >= 10.0
                            && p_raw >= p0 * 1.6)
                    {
                        Some(TrendBadge::Revival)
                    } else {
                        Some(TrendBadge::Rising)
                    }
                } else if baseline_games >= min_move_games && delta_shrunk <= -5.0 && z <= -1.2 {
                    Some(TrendBadge::Falling)
                } else if recent_games.saturating_add(baseline_games) >= min_move_games {
                    Some(TrendBadge::Steady)
                } else {
                    None
                };
                (badge, conf)
            };

            let buckets = (spark_start..TREND_BUCKETS)
                .map(|idx| {
                    let age = (TREND_BUCKETS - 1 - idx) as u32;
                    let quarter = pos_anchor.saturating_sub(age);
                    let move_games = m.buckets[idx];
                    let position_games = pos_buckets[idx];
                    let share = (position_games > 0)
                        .then(|| f64::from(move_games) / f64::from(position_games));
                    TrendBucket {
                        quarter,
                        move_games,
                        position_games,
                        share,
                    }
                })
                .collect();

            MoveTrend {
                mv: m.mv,
                recent_games,
                recent_total,
                recent_share,
                shrunk_recent_share,
                baseline_games,
                baseline_total,
                baseline_share,
                raw_delta_pp,
                shrunk_delta_pp,
                badge,
                confidence,
                buckets,
            }
        })
        .collect()
}

/// Convenience map from encoded move `mv` to its [`MoveTrend`].
pub fn position_trend_map(moves: &[MoveStat], config: TrendConfig) -> HashMap<u32, MoveTrend> {
    position_trends(moves, config)
        .into_iter()
        .map(|t| (t.mv, t))
        .collect()
}

fn align_move_stat(stat: &mut MoveStat, new_q: u32) {
    if new_q == 0 {
        return;
    }
    if stat.anchor_quarter == 0 {
        stat.anchor_quarter = new_q;
        return;
    }
    if new_q <= stat.anchor_quarter {
        return;
    }
    let shift = (new_q - stat.anchor_quarter) as usize;
    if shift >= TREND_BUCKETS {
        let moved: u32 = stat.buckets.iter().sum();
        stat.older_dated = stat.older_dated.saturating_add(moved);
        stat.buckets = [0; TREND_BUCKETS];
    } else {
        let moved: u32 = stat.buckets[..shift].iter().sum();
        stat.older_dated = stat.older_dated.saturating_add(moved);
        stat.buckets.copy_within(shift..TREND_BUCKETS, 0);
        stat.buckets[TREND_BUCKETS - shift..].fill(0);
    }
    stat.anchor_quarter = new_q;
}

/// A compact position tree built from games that are already in memory.
#[derive(Clone, Debug, Default)]
pub struct RecentTree {
    positions: HashMap<u64, BTreeMap<u32, RecentMove>>,
    games: usize,
    as_of: u32,
    recent_days: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct RecentMove {
    games: u32,
    white: u32,
    draw: u32,
    black: u32,
    elo_sum: u64,
    elo_count: u32,
    first_date: u32,
    last_date: u32,
    recent_games: u32,
    older_games: u32,
    anchor_quarter: u32,
    older_dated: u32,
    buckets: [u32; TREND_BUCKETS],
}

impl RecentMove {
    fn add(&mut self, result: &str, elo: Option<u32>, date: u32, recent: bool) {
        self.games += 1;
        match result {
            "1-0" => self.white += 1,
            "0-1" => self.black += 1,
            "1/2-1/2" => self.draw += 1,
            _ => {}
        }
        if let Some(elo) = elo {
            self.elo_sum += u64::from(elo);
            self.elo_count += 1;
        }
        if date != 0 {
            if self.first_date == 0 || date < self.first_date {
                self.first_date = date;
            }
            self.last_date = self.last_date.max(date);
            if let Some(q) = date_to_quarter(date) {
                self.add_quarter(q);
            }
        }
        if recent {
            self.recent_games += 1;
        } else {
            self.older_games += 1;
        }
    }

    fn add_quarter(&mut self, q: u32) {
        if q == 0 {
            return;
        }
        if self.anchor_quarter == 0 {
            self.anchor_quarter = q;
            self.buckets[TREND_BUCKETS - 1] = self.buckets[TREND_BUCKETS - 1].saturating_add(1);
        } else if q > self.anchor_quarter {
            let shift = (q - self.anchor_quarter) as usize;
            if shift >= TREND_BUCKETS {
                let moved: u32 = self.buckets.iter().sum();
                self.older_dated = self.older_dated.saturating_add(moved);
                self.buckets = [0; TREND_BUCKETS];
            } else {
                let moved: u32 = self.buckets[..shift].iter().sum();
                self.older_dated = self.older_dated.saturating_add(moved);
                self.buckets.copy_within(shift..TREND_BUCKETS, 0);
                self.buckets[TREND_BUCKETS - shift..].fill(0);
            }
            self.anchor_quarter = q;
            self.buckets[TREND_BUCKETS - 1] = self.buckets[TREND_BUCKETS - 1].saturating_add(1);
        } else {
            let age = (self.anchor_quarter - q) as usize;
            if age < TREND_BUCKETS {
                let idx = TREND_BUCKETS - 1 - age;
                self.buckets[idx] = self.buckets[idx].saturating_add(1);
            } else {
                self.older_dated = self.older_dated.saturating_add(1);
            }
        }
    }

    fn stat(&self, key: u64, mv: u32) -> MoveStat {
        MoveStat {
            key,
            mv,
            games: self.games,
            white: self.white,
            draw: self.draw,
            black: self.black,
            elo_sum: self.elo_sum,
            elo_count: self.elo_count,
            first_date: self.first_date,
            last_date: self.last_date,
            anchor_quarter: self.anchor_quarter,
            older_dated: self.older_dated,
            buckets: self.buckets,
        }
    }
}

impl RecentTree {
    /// Build a tree from `games`. Games dated within `recent_days` of `as_of`
    /// are counted as recent; the split is retained for callers that want to
    /// show a within-sample trend.
    pub fn from_games(games: &[Game], as_of: u32, recent_days: u32) -> Self {
        let mut tree = Self {
            positions: HashMap::new(),
            games: 0,
            as_of,
            recent_days,
        };
        for game in games {
            tree.add_game(game);
        }
        tree
    }

    pub fn games(&self) -> usize {
        self.games
    }

    pub fn as_of(&self) -> u32 {
        self.as_of
    }

    pub fn recent_days(&self) -> u32 {
        self.recent_days
    }

    fn add_game(&mut self, game: &Game) {
        let date = players::parse_date(game.date()).unwrap_or(0);
        let recent = players::date_to_days(date)
            .zip(players::date_to_days(self.as_of))
            .is_some_and(|(date, as_of)| as_of.saturating_sub(date) <= i64::from(self.recent_days));
        let elo = average_elo(game);
        let result = game.result();
        let mut seen = HashSet::new();
        let mut node = 0usize;

        loop {
            let parent_pos = &game.nodes[node].pos;
            let key = position::position_key(parent_pos);
            if !seen.insert(key) {
                break;
            }
            let Some(&child) = game.nodes[node].children.first() else {
                break;
            };
            let Some(ply) = &game.nodes[child].mv else {
                break;
            };
            if ply.uci == "0000" {
                break;
            }
            let Ok(uci) = ply.uci.parse::<UciMove>() else {
                break;
            };
            let Ok(mv) = uci.to_move(parent_pos) else {
                break;
            };
            let Some(code) = position::encode_move(mv) else {
                break;
            };
            self.positions
                .entry(key)
                .or_default()
                .entry(code)
                .or_default()
                .add(result, elo, date, recent);
            node = child;
        }
        self.games += 1;
    }

    /// Moves from `key`, sorted by frequency (then move code), with quarterly
    /// buckets aligned to the position's latest dated game.
    pub fn moves(&self, key: u64) -> Vec<MoveStat> {
        let mut moves: Vec<MoveStat> = self
            .positions
            .get(&key)
            .map(|moves| moves.iter().map(|(&mv, stat)| stat.stat(key, mv)).collect())
            .unwrap_or_default();
        let pos_anchor = moves.iter().map(|m| m.anchor_quarter).max().unwrap_or(0);
        if pos_anchor != 0 {
            for m in &mut moves {
                align_move_stat(m, pos_anchor);
            }
        }
        moves.sort_by(|a, b| b.games.cmp(&a.games).then(a.mv.cmp(&b.mv)));
        moves
    }

    /// Aggregated statistics for a position, or `None` when no game moved from
    /// it.
    pub fn stats(&self, key: u64) -> Option<PositionStats> {
        let moves = self.positions.get(&key)?;
        let mut stats = PositionStats::default();
        for mv in moves.values() {
            stats.games += mv.games;
            stats.white += mv.white;
            stats.draw += mv.draw;
            stats.black += mv.black;
            stats.elo_sum += mv.elo_sum;
            stats.elo_count += mv.elo_count;
            if mv.first_date != 0 && (stats.first_date == 0 || mv.first_date < stats.first_date) {
                stats.first_date = mv.first_date;
            }
            stats.last_date = stats.last_date.max(mv.last_date);
        }
        Some(stats)
    }

    /// Number of games that reached `key` and then made a move.
    pub fn position_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.games).sum())
            .unwrap_or(0)
    }

    /// Number of moves at `key` that fall in the recent window.
    pub fn recent_move_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.recent_games).sum())
            .unwrap_or(0)
    }

    /// Number of moves at `key` outside the recent window.
    pub fn older_move_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.older_games).sum())
            .unwrap_or(0)
    }
}

/// One move's recent share compared with a baseline distribution.
#[derive(Clone, Debug, PartialEq)]
pub struct TrendRow {
    /// Recent move statistics (games, score, Elo, dates).
    pub stat: MoveStat,
    /// Games in the recent sample that played this move.
    pub recent_games: u32,
    /// Total recent games that moved from the position.
    pub recent_total: u32,
    /// Share of the recent sample, 0.0–1.0.
    pub recent_share: f64,
    /// Games in the baseline that played this move.
    pub baseline_games: u32,
    /// Total baseline games that moved from the position.
    pub baseline_total: u32,
    /// Baseline share, when the baseline has data.
    pub baseline_share: Option<f64>,
    /// Recent share minus baseline share, in percentage points.
    pub delta_pp: Option<f64>,
}

impl TrendRow {
    /// A short label for the direction of the trend.
    pub fn direction(&self) -> &'static str {
        match self.delta_pp {
            Some(delta) if delta >= 3.0 => "rising",
            Some(delta) if delta <= -3.0 => "falling",
            _ => "steady",
        }
    }
}

/// Compare recent move shares with a baseline. The result is sorted by the
/// size of the positive change, so a newly popular move appears first.
pub fn trends(tree: &RecentTree, key: u64, baseline: &[MoveStat]) -> Vec<TrendRow> {
    let recent_moves = tree.moves(key);
    if recent_moves.is_empty() {
        return Vec::new();
    }
    let recent_total: u32 = recent_moves.iter().map(|mv| mv.games).sum();
    if recent_total == 0 {
        return Vec::new();
    }
    let baseline_total: u32 = baseline.iter().map(|mv| mv.games).sum();
    let baseline_by_move: HashMap<u32, &MoveStat> =
        baseline.iter().map(|stat| (stat.mv, stat)).collect();

    let mut rows: Vec<TrendRow> = recent_moves
        .into_iter()
        .map(|stat| {
            let baseline_games = baseline_by_move
                .get(&stat.mv)
                .map_or(0, |baseline| baseline.games);
            let recent_share = f64::from(stat.games) / f64::from(recent_total);
            let baseline_share =
                (baseline_total > 0).then(|| f64::from(baseline_games) / f64::from(baseline_total));
            let delta_pp = baseline_share.map(|share| (recent_share - share) * 100.0);
            TrendRow {
                stat,
                recent_games: stat.games,
                recent_total,
                recent_share,
                baseline_games,
                baseline_total,
                baseline_share,
                delta_pp,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.delta_pp
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.delta_pp.unwrap_or(f64::NEG_INFINITY))
            .then(b.recent_games.cmp(&a.recent_games))
            .then(a.stat.mv.cmp(&b.stat.mv))
    });
    rows
}

fn average_elo(game: &Game) -> Option<u32> {
    let white = game.header("WhiteElo").and_then(parse_elo);
    let black = game.header("BlackElo").and_then(parse_elo);
    match (white, black) {
        (Some(white), Some(black)) => Some(((white + black) / 2).max(1)),
        (Some(elo), None) | (None, Some(elo)) => Some(elo),
        (None, None) => None,
    }
}

fn parse_elo(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|elo| *elo > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::{Chess, Position, SanPlus};

    fn pgn(games: &str) -> Vec<Game> {
        crate::domain::parse_pgn(games.as_bytes()).unwrap()
    }

    fn build_repeated_pgn(blocks: &[(&str, &str, usize)]) -> Vec<Game> {
        let mut text = String::new();
        for &(date, moves, count) in blocks {
            for i in 0..count {
                text.push_str(&format!(
                    "[Event \"T{i}\"]\n[Date \"{date}\"]\n[White \"W\"]\n[Black \"B\"]\n[Result \"1-0\"]\n\n{moves} 1-0\n\n"
                ));
            }
        }
        pgn(&text)
    }

    #[test]
    fn tree_aggregates_moves_and_scores() {
        let games = pgn(r#"[Event "A"]
[White "One"]
[Black "Two"]
[Date "2026.01.01"]
[WhiteElo "2500"]
[BlackElo "2400"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "B"]
[White "Three"]
[Black "Four"]
[Date "2026.02.01"]
[WhiteElo "2600"]
[BlackElo "2500"]
[Result "1/2-1/2"]

1. e4 c5 1/2-1/2
"#);
        let tree = RecentTree::from_games(&games, 20260301, 60);
        let root = position::position_key(&Chess::default());
        let moves = tree.moves(root);
        assert_eq!(moves.len(), 1);
        let e4 = moves.iter().find(|mv| mv.games == 2).unwrap();
        assert_eq!(e4.white, 1);
        assert_eq!(e4.draw, 1);
        assert_eq!(e4.elo_avg(), Some(2500.0));
        assert_eq!(tree.stats(root).unwrap().games, 2);
        assert_eq!(tree.position_games(root), 2);
        assert_eq!(tree.recent_move_games(root), 2);
    }

    #[test]
    fn trends_compare_recent_share_with_baseline() {
        let games = pgn(r#"[Event "A"]
[White "One"]
[Black "Two"]
[Date "2026.02.01"]
[Result "1-0"]

1. e4 e5 1-0

[Event "B"]
[White "Three"]
[Black "Four"]
[Date "2026.02.02"]
[Result "1-0"]

1. e4 e5 1-0

[Event "C"]
[White "Five"]
[Black "Six"]
[Date "2026.02.03"]
[Result "1-0"]

1. d4 d5 1-0
"#);
        let tree = RecentTree::from_games(&games, 20260301, 60);
        let root = position::position_key(&Chess::default());
        let baseline = tree.moves(root);
        let rows = trends(&tree, root, &baseline);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].recent_games, 2);
        assert_eq!(rows[0].delta_pp, Some(0.0));
        assert_eq!(rows[1].delta_pp, Some(0.0));
    }

    #[test]
    fn detects_new_idea_and_declining_move() {
        // Historical (2023–2024): 30 games of 1. e4, 10 games of 1. d4, 0 of 1. c4.
        // Recent 12m (2026): 6 games of 1. e4, 6 games of 1. d4, 12 games of 1. c4 (new!).
        let games = build_repeated_pgn(&[
            ("2023.05.10", "1. e4 e5", 15),
            ("2024.05.10", "1. e4 e5", 15),
            ("2024.08.10", "1. d4 d5", 10),
            ("2026.02.15", "1. e4 e5", 6),
            ("2026.05.15", "1. d4 d5", 6),
            ("2026.08.15", "1. c4 c5", 12),
        ]);
        let tree = RecentTree::from_games(&games, 20260901, 365);
        let start = Chess::default();
        let root = position::position_key(&start);
        let moves = tree.moves(root);
        let trend_map = position_trend_map(&moves, TrendConfig::default());

        let c4_stat = moves
            .iter()
            .find(|m| m.san(&start).as_deref() == Some("c4"))
            .unwrap();
        let e4_stat = moves
            .iter()
            .find(|m| m.san(&start).as_deref() == Some("e4"))
            .unwrap();

        let c4_trend = &trend_map[&c4_stat.mv];
        assert_eq!(c4_trend.badge, Some(TrendBadge::New));
        assert_eq!(c4_trend.recent_games, 12);
        assert_eq!(c4_trend.baseline_games, 0);
        assert!(c4_trend.shrunk_delta_pp.unwrap() > 25.0);

        let e4_trend = &trend_map[&e4_stat.mv];
        assert_eq!(e4_trend.badge, Some(TrendBadge::Falling));
        assert_eq!(e4_trend.recent_games, 6);
        assert_eq!(e4_trend.baseline_games, 30);
        assert!(e4_trend.shrunk_delta_pp.unwrap() < -20.0);
    }

    #[test]
    fn distinguishes_revival_from_brand_new_idea() {
        // 1. b3 was played in 2021–2022 (6 games), disappeared in 2024–2025 (0 games
        // while 1. e4 was played 30 times), and surged back in 2026 (10 games out of 20).
        let games = build_repeated_pgn(&[
            ("2021.06.01", "1. b3 e5", 6),
            ("2022.06.01", "1. e4 e5", 14),
            ("2024.06.01", "1. e4 e5", 15),
            ("2025.02.01", "1. d4 d5", 15),
            ("2026.04.01", "1. b3 e5", 10),
            ("2026.07.01", "1. e4 e5", 10),
        ]);
        let tree = RecentTree::from_games(&games, 20260901, 365);
        let start = Chess::default();
        let root = position::position_key(&start);
        let moves = tree.moves(root);
        let trend_map = position_trend_map(&moves, TrendConfig::default());

        let b3_stat = moves
            .iter()
            .find(|m| m.san(&start).as_deref() == Some("b3"))
            .unwrap();
        let b3_trend = &trend_map[&b3_stat.mv];
        assert_eq!(b3_trend.badge, Some(TrendBadge::Revival));
    }

    #[test]
    fn suppresses_small_sample_false_positives() {
        // Case 1: Only 2 recent games at the position (both playing 1. g4 after 40 historical games).
        let games_sparse = build_repeated_pgn(&[
            ("2023.04.01", "1. e4 e5", 40),
            ("2026.07.01", "1. g4 d5", 2),
        ]);
        let tree_sparse = RecentTree::from_games(&games_sparse, 20260901, 365);
        let start = Chess::default();
        let root = position::position_key(&start);
        let moves_sparse = tree_sparse.moves(root);
        let map_sparse = position_trend_map(&moves_sparse, TrendConfig::default());
        for trend in map_sparse.values() {
            assert_eq!(
                trend.badge, None,
                "a 2-game recent window must never emit a trend badge"
            );
        }

        // Case 2: 20 recent games at the position, where 1. g4 was played twice (2/20 = 10% raw).
        let games_fluke = build_repeated_pgn(&[
            ("2023.04.01", "1. e4 e5", 40),
            ("2026.06.01", "1. e4 e5", 18),
            ("2026.07.01", "1. g4 d5", 2),
        ]);
        let tree_fluke = RecentTree::from_games(&games_fluke, 20260901, 365);
        let moves_fluke = tree_fluke.moves(root);
        let map_fluke = position_trend_map(&moves_fluke, TrendConfig::default());
        let g4_stat = moves_fluke
            .iter()
            .find(|m| m.san(&start).as_deref() == Some("g4"))
            .unwrap();
        assert_eq!(
            map_fluke[&g4_stat.mv].badge, None,
            "a 2-game fluke inside a 20-game recent window must not be flagged as New or Rising"
        );
    }

    #[test]
    fn aggregates_trends_across_transpositions() {
        // Reach the QGD 1. d4 Nf6 2. c4 e6 3. Nf3 d5 position via two different move orders:
        // Order A: 1. d4 Nf6 2. c4 e6 3. Nf3 d5
        // Order B: 1. Nf3 Nf6 2. c4 e6 3. d4 d5
        // At that transposed position, White historically played 4. Nc3 (20 games in 2023),
        // while in 2026 White switched to 4. g3 (Catalan transposition, 12 games across both orders).
        let games = build_repeated_pgn(&[
            ("2023.05.01", "1. d4 Nf6 2. c4 e6 3. Nf3 d5 4. Nc3 Be7", 10),
            ("2023.08.01", "1. Nf3 Nf6 2. c4 e6 3. d4 d5 4. Nc3 Be7", 10),
            ("2026.03.01", "1. d4 Nf6 2. c4 e6 3. Nf3 d5 4. g3 Be7", 6),
            ("2026.06.01", "1. Nf3 Nf6 2. c4 e6 3. d4 d5 4. g3 Be7", 6),
            ("2026.07.01", "1. d4 Nf6 2. c4 e6 3. Nf3 d5 4. Nc3 Be7", 3),
        ]);
        let tree = RecentTree::from_games(&games, 20260901, 365);
        let mut pos = Chess::default();
        for token in ["d4", "Nf6", "c4", "e6", "Nf3", "d5"] {
            let san = SanPlus::from_ascii(token.as_bytes()).unwrap();
            let mv = san.san.to_move(&pos).unwrap();
            pos = pos.play(mv).unwrap();
        }
        let key = position::position_key(&pos);
        let moves = tree.moves(key);
        assert_eq!(moves.len(), 2);
        let trend_map = position_trend_map(&moves, TrendConfig::default());

        let g3_stat = moves
            .iter()
            .find(|m| m.san(&pos).as_deref() == Some("g3"))
            .unwrap();
        let g3_trend = &trend_map[&g3_stat.mv];
        assert_eq!(g3_trend.recent_games, 12);
        assert_eq!(g3_trend.recent_total, 15);
        assert_eq!(g3_trend.baseline_total, 20);
        assert_eq!(g3_trend.badge, Some(TrendBadge::New));
    }

    #[test]
    fn sparkline_points_normalize_chronological_quarters_and_handle_sparse_buckets() {
        // Declining 1. e4 share: dominant in 2023-2024, rare in 2026.
        let games = build_repeated_pgn(&[
            ("2023.05.10", "1. e4 e5", 15),
            ("2024.05.10", "1. e4 e5", 15),
            ("2024.08.10", "1. d4 d5", 10),
            ("2026.02.15", "1. e4 e5", 6),
            ("2026.05.15", "1. d4 d5", 6),
            ("2026.08.15", "1. c4 c5", 12),
        ]);
        let tree = RecentTree::from_games(&games, 20260901, 365);
        let start = Chess::default();
        let root = position::position_key(&start);
        let moves = tree.moves(root);
        let trend_map = position_trend_map(&moves, TrendConfig::default());
        let e4_stat = moves
            .iter()
            .find(|m| m.san(&start).as_deref() == Some("e4"))
            .unwrap();
        let e4_trend = &trend_map[&e4_stat.mv];
        assert_eq!(e4_trend.badge, Some(TrendBadge::Falling));
        let pts = e4_trend.sparkline_points();
        assert!(pts.len() >= 2, "declining trend needs >= 2 points");
        for (i, (x, y)) in pts.iter().enumerate() {
            assert!((0.0..=1.0).contains(x), "x in 0..1 (idx {i})");
            assert!((0.0..=1.0).contains(y), "y in 0..1 (idx {i})");
            if i > 0 {
                assert!(pts[i - 1].0 < pts[i].0, "x must increase chronologically");
            }
        }
        assert!(
            pts.first().unwrap().1 > pts.last().unwrap().1,
            "declining share: first y ({}) > last y ({})",
            pts.first().unwrap().1,
            pts.last().unwrap().1
        );
        if let Some(by) = e4_trend.sparkline_baseline_y() {
            assert!((0.0..=1.0).contains(&by), "baseline y in 0..1");
        }

        // Flat steady shares stay centered near 0.5.
        let flat = MoveTrend {
            mv: 0,
            recent_games: 10,
            recent_total: 20,
            recent_share: Some(0.5),
            shrunk_recent_share: Some(0.5),
            baseline_games: 20,
            baseline_total: 40,
            baseline_share: Some(0.5),
            raw_delta_pp: Some(0.0),
            shrunk_delta_pp: Some(0.0),
            badge: Some(TrendBadge::Steady),
            confidence: TrendConfidence::Low,
            buckets: (0..8)
                .map(|i| TrendBucket {
                    quarter: i,
                    move_games: 5,
                    position_games: 10,
                    share: Some(0.5),
                })
                .collect(),
        };
        let flat_pts = flat.sparkline_points();
        assert_eq!(flat_pts.len(), 8);
        for (_, y) in &flat_pts {
            assert!((y - 0.5).abs() < 0.05, "flat shares map near 0.5, got {y}");
        }

        // Sparse: badge None or a single active bucket yields empty points.
        let sparse_none = MoveTrend {
            mv: 1,
            recent_games: 2,
            recent_total: 2,
            recent_share: Some(1.0),
            shrunk_recent_share: Some(1.0),
            baseline_games: 40,
            baseline_total: 40,
            baseline_share: Some(0.0),
            raw_delta_pp: Some(100.0),
            shrunk_delta_pp: Some(50.0),
            badge: None,
            confidence: TrendConfidence::Low,
            buckets: vec![TrendBucket {
                quarter: 0,
                move_games: 2,
                position_games: 2,
                share: Some(1.0),
            }],
        };
        assert!(sparse_none.sparkline_points().is_empty());
        assert_eq!(sparse_none.sparkline_baseline_y(), None);
        let mut single_buckets = vec![
            TrendBucket {
                quarter: 0,
                move_games: 0,
                position_games: 0,
                share: None,
            };
            8
        ];
        single_buckets[7] = TrendBucket {
            quarter: 7,
            move_games: 3,
            position_games: 5,
            share: Some(0.6),
        };
        let sparse_single = MoveTrend {
            mv: 2,
            recent_games: 3,
            recent_total: 5,
            recent_share: Some(0.6),
            shrunk_recent_share: Some(0.6),
            baseline_games: 10,
            baseline_total: 20,
            baseline_share: Some(0.5),
            raw_delta_pp: Some(10.0),
            shrunk_delta_pp: Some(10.0),
            badge: Some(TrendBadge::Steady),
            confidence: TrendConfidence::Low,
            buckets: single_buckets,
        };
        assert!(sparse_single.sparkline_points().is_empty());
        assert_eq!(sparse_single.sparkline_baseline_y(), None);
    }
}
