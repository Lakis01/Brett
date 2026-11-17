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
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluated {
    /// UCI score from the side to move.
    pub eval: Option<Score>,
    /// Engine's preferred move in UCI notation.
    pub best: Option<String>,
    /// White-perspective centipawn value, with mates compressed near ±10000.
    pub white_cp: Option<f64>,
    /// Principal variation for the best move (UCI moves).
    pub pv: Vec<String>,
    /// Score of the second-best move (MultiPV 2) from the side to move, used to
    /// measure position criticality.
    pub second_eval: Option<Score>,
    /// Search depth that produced this evaluation.
    pub depth: Option<u32>,
    /// True when restored directly from `analysis.db`.
    pub from_cache: bool,
}

/// Coach-grade clock-management pattern detected by crossing position
/// criticality, clock effort, and win-probability loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeManagementTag {
    /// High criticality or mistake/blunder played much faster than the player's
    /// median move time while plenty of clock remained.
    Rushed,
    /// Long think spent on a low-criticality or already-decisive position.
    Overthink,
    /// Mistake or inaccuracy committed under severe clock pressure.
    TimeTrouble,
    /// Deep think on a critical position that found the best move.
    Clutch,
}

impl TimeManagementTag {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Rushed => "⚡ Rushed",
            Self::Overthink => "⏳ Overthink",
            Self::TimeTrouble => "🔥 Time trouble",
            Self::Clutch => "🎯 Clutch",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Rushed => "⚡",
            Self::Overthink => "⏳",
            Self::TimeTrouble => "🔥",
            Self::Clutch => "🎯",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Rushed => {
                "Impulsive critical mistake: played quickly with comfortable clock when a critical decision existed"
            }
            Self::Overthink => {
                "Perfectionist time burn: long think in a flat or already-decisive position"
            }
            Self::TimeTrouble => "Inaccuracy or mistake committed under severe clock pressure",
            Self::Clutch => "Earned deep think on a critical position that found the best move",
        }
    }
}

/// Corpus-level clock pacing summary across a player's clocked games.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerClockSummary {
    pub clocked_games: usize,
    pub pct_at_move_10: Option<f64>,
    pub pct_at_move_20: Option<f64>,
    pub pct_at_move_30: Option<f64>,
    pub pct_at_move_40: Option<f64>,
    pub time_trouble_rate: f64,
    /// `(median_move_number, avg_think_secs)` where the player exits fast
    /// opening recall into their first long think.
    pub prep_exit_move: Option<(u32, f64)>,
}

#[derive(Clone, Debug)]
struct ReviewPosition {
    fen: String,
    turn: Color,
    pos_hash: u64,
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
    /// Principal variation for the engine's best move from the parent position.
    pub best_pv: Vec<String>,
    /// Centipawn loss from the mover's point of view.
    pub cp_loss: Option<i32>,
    /// Mover's win probability before the move (`0.0..=100.0`).
    pub win_before: Option<f64>,
    /// Mover's win probability after the move (`0.0..=100.0`).
    pub win_after: Option<f64>,
    /// Per-move Lichess accuracy (`0.0..=100.0`); `None` for forced moves.
    pub accuracy: Option<f64>,
    pub quality: Quality,
    /// Win-probability drop (`0.0..=100.0`) between the best move (PV 1) and
    /// second-best move (PV 2) before this move was played.
    pub criticality: Option<f64>,
    /// True when the position was critical (`criticality >= 15%`) and the
    /// player found the singular best move.
    pub is_only_move_great: bool,
    /// Time spent on this move in seconds, when `[%clk]` annotations exist.
    pub move_time_secs: Option<f64>,
    /// Remaining clock after this move in seconds.
    pub clock_remaining_secs: Option<f64>,
    /// Share of remaining effective time budget spent on this move (`0.0..=1.0`).
    pub clock_effort: Option<f64>,
    /// Coach-grade time management classification, when applicable.
    pub time_tag: Option<TimeManagementTag>,
    /// True when the parent evaluation came from `analysis.db`.
    pub from_cache: bool,
}

impl Default for MoveReview {
    fn default() -> Self {
        Self {
            node: 0,
            number: 1,
            color: Color::White,
            san: String::new(),
            played: String::new(),
            eval_before: None,
            eval_after: None,
            best: None,
            best_pv: Vec::new(),
            cp_loss: None,
            win_before: None,
            win_after: None,
            accuracy: None,
            quality: Quality::Unknown,
            criticality: None,
            is_only_move_great: false,
            move_time_secs: None,
            clock_remaining_secs: None,
            clock_effort: None,
            time_tag: None,
            from_cache: false,
        }
    }
}

impl MoveReview {
    /// Decisive evaluation swings, critical only-moves, and notable clock leaks.
    pub fn is_key_moment(&self) -> bool {
        matches!(
            self.quality,
            Quality::Inaccuracy | Quality::Mistake | Quality::Blunder
        ) || self.is_only_move_great
            || self.time_tag.is_some()
    }
}

/// Per-color game accuracy, phase breakdown, time discipline, and ACPL summary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AccuracySummary {
    pub white: Option<f64>,
    pub black: Option<f64>,
    pub white_acpl: Option<f64>,
    pub black_acpl: Option<f64>,
    pub white_moves: usize,
    pub black_moves: usize,
    pub white_opening: Option<f64>,
    pub black_opening: Option<f64>,
    pub white_middlegame: Option<f64>,
    pub black_middlegame: Option<f64>,
    pub white_endgame: Option<f64>,
    pub black_endgame: Option<f64>,
    pub white_time_discipline: Option<f64>,
    pub black_time_discipline: Option<f64>,
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
    last_infos: std::collections::BTreeMap<usize, EngineInfo>,
    pending_cache_writes: Vec<crate::analysis_cache::CachedAnalysis>,
    two_pass_enabled: bool,
    deepen_queue: std::collections::VecDeque<usize>,
    in_deepen_pass: bool,
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
                    pos_hash: position.position_hash(),
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
            last_infos: std::collections::BTreeMap::new(),
            pending_cache_writes: Vec::new(),
            two_pass_enabled: false,
            deepen_queue: std::collections::VecDeque::new(),
            in_deepen_pass: false,
        }
    }

    /// Enable automatic Pass-2 deepening on the game's largest evaluation swings.
    pub fn enable_two_pass_deepening(&mut self) {
        self.two_pass_enabled = true;
    }

    /// True while Pass 2 is deepening critical swing positions.
    pub fn is_deepening(&self) -> bool {
        self.in_deepen_pass
    }

    /// Pre-populate positions from `analysis.db` when a Stockfish/Generic entry
    /// meets `min_depth`. Returns the number of positions satisfied from cache.
    pub fn seed_from_analysis_cache(
        &mut self,
        cache: &crate::analysis_cache::AnalysisCacheHandle,
        min_depth: u32,
    ) -> usize {
        let mut hits = 0usize;
        for (idx, pos) in self.positions.iter().enumerate() {
            let cached = cache.query_position(pos.pos_hash);
            let pv1 = cached.iter().find(|entry| {
                entry.multipv == 1
                    && entry.depth >= min_depth
                    && matches!(
                        entry.engine_family,
                        crate::analysis_cache::EngineFamily::Stockfish
                            | crate::analysis_cache::EngineFamily::Generic
                    )
            });
            let Some(pv1) = pv1 else {
                continue;
            };
            let eval = if let Some(mate) = pv1.score_mate {
                Some(Score::Mate(mate))
            } else {
                pv1.score_cp.map(Score::Cp)
            };
            let second_eval = cached
                .iter()
                .find(|entry| entry.multipv == 2 && entry.engine_family == pv1.engine_family)
                .and_then(|pv2| {
                    if let Some(mate) = pv2.score_mate {
                        Some(Score::Mate(mate))
                    } else {
                        pv2.score_cp.map(Score::Cp)
                    }
                });
            self.evaluated[idx] = Some(Evaluated {
                eval,
                best: pv1.pv.first().cloned(),
                white_cp: eval.map(|s| score_white_cp(s, pos.turn)),
                pv: pv1.pv.clone(),
                second_eval,
                depth: Some(pv1.depth),
                from_cache: true,
            });
            hits += 1;
        }
        while self.next < self.positions.len() && self.evaluated[self.next].is_some() {
            self.next += 1;
        }
        if self.next >= self.positions.len() {
            self.running = false;
        }
        hits
    }

    /// FEN of the next position that still needs an evaluation.
    pub fn next_pending_fen(&self) -> Option<&str> {
        if !self.running || self.in_deepen_pass {
            return None;
        }
        let idx = (self.next..self.positions.len()).find(|&i| self.evaluated[i].is_none())?;
        Some(self.positions[idx].fen.as_str())
    }

    /// Fill a position from a Lichess Cloud Eval result. Cloud scores are in
    /// White's perspective; the review state stores UCI side-to-move scores.
    /// Returns true when a matching pending position was filled.
    pub fn seed_from_cloud(
        &mut self,
        fen: &str,
        eval: Option<Score>,
        best: Option<String>,
        pv: Vec<String>,
        second_eval: Option<Score>,
        depth: Option<u32>,
    ) -> bool {
        let Some(idx) = self.positions.iter().enumerate().find_map(|(idx, pos)| {
            (pos.fen == fen && self.evaluated[idx].is_none()).then_some(idx)
        }) else {
            return false;
        };
        let turn = self.positions[idx].turn;
        let side_to_move = |score: Score| match turn {
            Color::White => score,
            Color::Black => match score {
                Score::Cp(cp) => Score::Cp(cp.saturating_neg()),
                Score::Mate(mate) => Score::Mate(mate.saturating_neg()),
            },
        };
        self.evaluated[idx] = Some(Evaluated {
            eval: eval.map(side_to_move),
            best,
            white_cp: eval.map(|score| score_white_cp(score, Color::White)),
            pv,
            second_eval: second_eval.map(side_to_move),
            depth,
            from_cache: true,
        });
        while self.next < self.positions.len() && self.evaluated[self.next].is_some() {
            self.next += 1;
        }
        if self.next >= self.positions.len() && !self.in_deepen_pass && !self.two_pass_enabled {
            self.running = false;
        }
        true
    }

    /// Drain newly completed position evaluations to persist into `analysis.db`.
    pub fn take_pending_cache_writes(
        &mut self,
        engine_name: &str,
    ) -> Vec<crate::analysis_cache::CachedAnalysis> {
        let name = if engine_name.trim().is_empty() {
            "Stockfish"
        } else {
            engine_name.trim()
        };
        let identity = crate::analysis_cache::parse_engine_identity(name);
        let mut out = std::mem::take(&mut self.pending_cache_writes);
        for item in &mut out {
            item.engine_family = identity.family;
            item.engine_name = name.to_string();
            item.quality_score = crate::analysis_cache::compute_quality_rating(
                identity.family,
                identity.baseline_elo,
                item.depth,
                item.seldepth,
                item.nodes,
                item.score_mate,
            );
        }
        out
    }

    /// Live engine info for the position currently being searched by the review.
    pub fn current_search_eval(&self) -> Option<(&str, Color, &EngineInfo)> {
        if !self.running {
            return None;
        }
        let target_idx = if self.in_deepen_pass {
            *self.deepen_queue.front()?
        } else {
            self.next
        };
        let pos = self.positions.get(target_idx)?;
        let info = self.last_info.as_ref()?;
        Some((pos.fen.as_str(), pos.turn, info))
    }

    pub fn total(&self) -> usize {
        self.positions.len()
    }

    pub fn progress(&self) -> (usize, usize) {
        let done = self.evaluated.iter().filter(|e| e.is_some()).count();
        (
            done.max(self.next).min(self.positions.len()),
            self.positions.len(),
        )
    }

    pub fn complete(&self) -> bool {
        !self.running && self.next >= self.positions.len()
    }

    /// Mark every position analyzed without an engine. Test-only: lets cache
    /// tests build a completed review cheaply.
    #[cfg(test)]
    pub(crate) fn mark_complete_for_test(&mut self) {
        self.evaluated.fill(Some(Evaluated::default()));
        self.next = self.positions.len();
        self.running = false;
        self.request = None;
    }

    fn build_deepen_queue(&mut self) {
        let mut swings: Vec<(usize, f64)> = Vec::new();
        for idx in 1..self.positions.len() {
            let (Some(before), Some(after)) = (
                self.evaluated[idx - 1].as_ref(),
                self.evaluated[idx].as_ref(),
            ) else {
                continue;
            };
            let (Some(b_score), Some(a_score)) = (before.eval, after.eval) else {
                continue;
            };
            let mover = self.positions[idx - 1].turn;
            let wb = WhiteScore::from_side_to_move(b_score, mover).win_percent_for(mover);
            let wa = WhiteScore::from_side_to_move(a_score, mover.other()).win_percent_for(mover);
            let drop = (wb - wa).abs();
            if drop >= 12.0 {
                if before.depth.unwrap_or(0) < 18 {
                    swings.push((idx - 1, drop));
                }
                if after.depth.unwrap_or(0) < 18 {
                    swings.push((idx, drop));
                }
            }
        }
        swings.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut seen = std::collections::HashSet::new();
        for (pos_idx, _) in swings {
            if seen.insert(pos_idx) {
                self.deepen_queue.push_back(pos_idx);
                if self.deepen_queue.len() >= 6 {
                    break;
                }
            }
        }
    }

    fn deepen_limit(&self) -> SearchLimit {
        match self.config.limit {
            SearchLimit::Depth(d) => SearchLimit::Depth(d.saturating_add(4).clamp(16, 22)),
            SearchLimit::MoveTime(ms) => SearchLimit::MoveTime(ms.saturating_mul(2).clamp(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(4_000),
            )),
            SearchLimit::Infinite => SearchLimit::Depth(18),
        }
    }

    /// Ask the engine for the next position. Returns true when a request was
    /// queued.
    pub fn request_next(&mut self, engine: &mut EngineHandle) -> bool {
        if !self.running {
            return false;
        }
        while self.next < self.positions.len() && self.evaluated[self.next].is_some() {
            self.next += 1;
        }
        if self.next < self.positions.len() {
            let position = &self.positions[self.next];
            self.last_info = None;
            self.last_infos.clear();
            self.request = Some(engine.analyze(position.fen.clone(), 2, self.config.limit));
            return true;
        }
        if self.two_pass_enabled && !self.in_deepen_pass {
            self.in_deepen_pass = true;
            self.build_deepen_queue();
        }
        if let Some(&target_idx) = self.deepen_queue.front() {
            let position = &self.positions[target_idx];
            self.last_info = None;
            self.last_infos.clear();
            self.request = Some(engine.analyze(position.fen.clone(), 2, self.deepen_limit()));
            return true;
        }
        self.running = false;
        false
    }

    pub fn note_info(&mut self, request: u64, info: EngineInfo) {
        if self.request == Some(request) {
            let mpv = info.multipv.max(1);
            if mpv == 1 || self.last_info.is_none() {
                self.last_info = Some(info.clone());
            }
            self.last_infos.insert(mpv, info);
        }
    }

    /// Consume the engine's `bestmove` for the current request. Returns true
    /// when it belonged to this review.
    pub fn finish_search(&mut self, request: u64, best: &str) -> bool {
        if self.request != Some(request) {
            return false;
        }
        let target_idx = if self.in_deepen_pass {
            let Some(idx) = self.deepen_queue.pop_front() else {
                self.running = false;
                self.request = None;
                return false;
            };
            idx
        } else {
            if self.next >= self.positions.len() {
                return false;
            }
            let idx = self.next;
            self.next += 1;
            idx
        };

        let pv1_info = self
            .last_infos
            .remove(&1)
            .or_else(|| self.last_info.take())
            .unwrap_or_default();
        let pv2_info = self.last_infos.remove(&2);
        self.last_info = None;
        self.last_infos.clear();

        let position = &self.positions[target_idx];
        let best_uci = if !pv1_info.pv.is_empty() {
            pv1_info.pv.first().cloned()
        } else {
            (best != "(none)").then(|| best.to_string())
        };
        let mut pv = pv1_info.pv.clone();
        if pv.is_empty()
            && let Some(b) = &best_uci
        {
            pv.push(b.clone());
        }

        if pv1_info.depth > 0 && pv1_info.score.is_some() {
            for (mpv, info) in
                std::iter::once((1usize, &pv1_info)).chain(pv2_info.as_ref().map(|i| (2usize, i)))
            {
                let (score_cp, score_mate) = match info.score {
                    Some(Score::Cp(cp)) => (Some(cp), None),
                    Some(Score::Mate(m)) => (None, Some(m)),
                    None => continue,
                };
                if info.pv.is_empty() && mpv > 1 {
                    continue;
                }
                let line = if info.pv.is_empty() {
                    pv.clone()
                } else {
                    info.pv.clone()
                };
                if line.is_empty() {
                    continue;
                }
                self.pending_cache_writes
                    .push(crate::analysis_cache::CachedAnalysis {
                        pos_hash: position.pos_hash,
                        fen: position.fen.clone(),
                        engine_family: crate::analysis_cache::EngineFamily::Stockfish,
                        engine_name: String::new(),
                        multipv: mpv,
                        depth: info.depth,
                        seldepth: info.seldepth,
                        score_cp,
                        score_mate,
                        nodes: info.nodes,
                        time_ms: info.time_ms,
                        nps: info.nps,
                        pv: line,
                        quality_score: 0.0,
                        analyzed_at: 0,
                    });
            }
        }

        self.evaluated[target_idx] = Some(Evaluated {
            eval: pv1_info.score,
            best: best_uci,
            white_cp: pv1_info
                .score
                .map(|score| score_white_cp(score, position.turn)),
            pv,
            second_eval: pv2_info.and_then(|i| i.score),
            depth: (pv1_info.depth > 0).then_some(pv1_info.depth),
            from_cache: false,
        });

        while self.next < self.positions.len() && self.evaluated[self.next].is_some() {
            self.next += 1;
        }
        self.request = None;
        if self.next >= self.positions.len() {
            if self.two_pass_enabled && !self.in_deepen_pass {
                self.in_deepen_pass = true;
                self.build_deepen_queue();
            }
            if !self.in_deepen_pass || self.deepen_queue.is_empty() {
                self.running = false;
            }
        }
        true
    }

    pub fn cancel(&mut self) {
        self.running = false;
        self.request = None;
        self.deepen_queue.clear();
    }

    /// Build move reviews from the positions analyzed so far.
    pub fn moves(&self, game: &Game) -> Vec<MoveReview> {
        let clock_map = extract_game_move_clocks(game);
        let median_think = median_move_times_by_color(&clock_map);
        let tc_info = parse_time_control_header(game.header("TimeControl"));
        let trouble_threshold = time_trouble_threshold_secs(tc_info.0);

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
            let criticality = match (before.eval, before.second_eval) {
                (Some(pv1_s), Some(pv2_s)) => {
                    let w1 =
                        WhiteScore::from_side_to_move(pv1_s, ply.color).win_percent_for(ply.color);
                    let w2 =
                        WhiteScore::from_side_to_move(pv2_s, ply.color).win_percent_for(ply.color);
                    Some((w1 - w2).max(0.0))
                }
                _ => None,
            };
            let (quality, cp_loss, win_before, win_after, accuracy) = if legal == 1 {
                (Quality::Forced, None, None, None, None)
            } else {
                let (mut quality, cp_loss) =
                    classify(&ply.uci, Some(before), Some(after), ply.color);
                let (win_before, win_after, accuracy) = match (before.eval, after.eval) {
                    (Some(before_score), Some(after_score)) => {
                        let wb = WhiteScore::from_side_to_move(before_score, ply.color)
                            .win_percent_for(ply.color);
                        let wa = WhiteScore::from_side_to_move(after_score, ply.color.other())
                            .win_percent_for(ply.color);
                        // Win-probability grading guard: in an already-decisive
                        // position (+5.0 -> +3.8), a tiny win-probability drop
                        // (< 3.5%) should not be flagged as a Mistake/Blunder.
                        let win_drop = (wb - wa).max(0.0);
                        if matches!(quality, Quality::Mistake | Quality::Blunder)
                            && win_drop < 3.5
                            && wb >= 92.0
                            && wa >= 89.0
                        {
                            quality = Quality::Good;
                        }
                        (Some(wb), Some(wa), Some(eval::move_accuracy(wb, wa)))
                    }
                    _ => (None, None, None),
                };
                (quality, cp_loss, win_before, win_after, accuracy)
            };
            let is_only_move_great =
                quality == Quality::Best && criticality.is_some_and(|k| k >= 15.0);
            let clk = clock_map.get(&node).copied();
            let move_time_secs = clk.and_then(|c| c.move_time_secs);
            let clock_remaining_secs = clk.map(|c| c.remaining_secs);
            let clock_effort = clk.and_then(|c| c.effort);
            let win_drop = match (win_before, win_after) {
                (Some(wb), Some(wa)) => (wb - wa).max(0.0),
                _ => 0.0,
            };
            let median = match ply.color {
                Color::White => median_think.0,
                Color::Black => median_think.1,
            };
            let time_tag = classify_time_tag(
                quality,
                criticality,
                win_drop,
                win_before,
                move_time_secs,
                clock_remaining_secs,
                clock_effort,
                median,
                trouble_threshold,
            );

            out.push(MoveReview {
                node,
                number: ply.number,
                color: ply.color,
                san: ply.san.clone(),
                played: ply.uci.clone(),
                eval_before: before.eval,
                eval_after: after.eval,
                best: before.best.clone(),
                best_pv: before.pv.clone(),
                cp_loss,
                win_before,
                win_after,
                accuracy,
                quality,
                criticality,
                is_only_move_great,
                move_time_secs,
                clock_remaining_secs,
                clock_effort,
                time_tag,
                from_cache: before.from_cache,
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
    let mut white_open = Vec::new();
    let mut black_open = Vec::new();
    let mut white_mid = Vec::new();
    let mut black_mid = Vec::new();
    let mut white_end = Vec::new();
    let mut black_end = Vec::new();
    let mut white_clocked = 0usize;
    let mut black_clocked = 0usize;
    let mut white_time_penalties = 0.0f64;
    let mut black_time_penalties = 0.0f64;

    for (idx, mov) in moves.iter().enumerate() {
        if mov.move_time_secs.is_some() {
            match mov.color {
                Color::White => {
                    white_clocked += 1;
                    match mov.time_tag {
                        Some(TimeManagementTag::Rushed) => white_time_penalties += 18.0,
                        Some(TimeManagementTag::Overthink) => white_time_penalties += 12.0,
                        Some(TimeManagementTag::TimeTrouble) => white_time_penalties += 10.0,
                        _ => {}
                    }
                }
                Color::Black => {
                    black_clocked += 1;
                    match mov.time_tag {
                        Some(TimeManagementTag::Rushed) => black_time_penalties += 18.0,
                        Some(TimeManagementTag::Overthink) => black_time_penalties += 12.0,
                        Some(TimeManagementTag::TimeTrouble) => black_time_penalties += 10.0,
                        _ => {}
                    }
                }
            }
        }
        if mov.quality == Quality::Forced {
            continue;
        }
        let weight = weights.get(idx).copied().unwrap_or(1.0);
        if let Some(acc) = mov.accuracy.or_else(|| {
            let (wb, wa) = (mov.win_before?, mov.win_after?);
            Some(eval::move_accuracy(wb, wa))
        }) {
            match mov.color {
                Color::White => {
                    white_weighted.push((acc, weight));
                    if mov.number <= 12 {
                        white_open.push((acc, weight));
                    } else if mov.number <= 30 {
                        white_mid.push((acc, weight));
                    } else {
                        white_end.push((acc, weight));
                    }
                }
                Color::Black => {
                    black_weighted.push((acc, weight));
                    if mov.number <= 12 {
                        black_open.push((acc, weight));
                    } else if mov.number <= 30 {
                        black_mid.push((acc, weight));
                    } else {
                        black_end.push((acc, weight));
                    }
                }
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
        white_opening: color_accuracy(&white_open),
        black_opening: color_accuracy(&black_open),
        white_middlegame: color_accuracy(&white_mid),
        black_middlegame: color_accuracy(&black_mid),
        white_endgame: color_accuracy(&white_end),
        black_endgame: color_accuracy(&black_end),
        white_time_discipline: (white_clocked >= 3)
            .then(|| (100.0 - white_time_penalties).clamp(0.0, 100.0)),
        black_time_discipline: (black_clocked >= 3)
            .then(|| (100.0 - black_time_penalties).clamp(0.0, 100.0)),
    }
}

#[derive(Clone, Copy, Debug)]
struct NodeClockStat {
    color: Color,
    remaining_secs: f64,
    move_time_secs: Option<f64>,
    effort: Option<f64>,
}

fn parse_time_control_header(tc: Option<&str>) -> (Option<f64>, f64) {
    let Some(raw) = tc
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "-" && *s != "?")
    else {
        return (None, 0.0);
    };
    let stage = raw.split(':').next().unwrap_or(raw);
    let after_moves = stage.rsplit('/').next().unwrap_or(stage);
    let mut parts = after_moves.split('+');
    let base = parts
        .next()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|&b| b > 0.0);
    let inc = parts
        .next()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(0.0);
    (base, inc)
}

fn time_trouble_threshold_secs(initial_secs: Option<f64>) -> f64 {
    match initial_secs {
        Some(base) if base >= 2_400.0 => 300.0,
        Some(base) if base >= 480.0 => 30.0,
        Some(_) => 10.0,
        None => 30.0,
    }
}

fn extract_game_move_clocks(game: &Game) -> std::collections::HashMap<usize, NodeClockStat> {
    let mut map = std::collections::HashMap::new();
    let (initial_base, inc) = parse_time_control_header(game.header("TimeControl"));
    let mut prev_white = initial_base;
    let mut prev_black = initial_base;

    for node in game.mainline() {
        let Some(ply) = game.nodes[node].mv.as_ref() else {
            continue;
        };
        let Some(clk) = game.nodes[node].clock else {
            continue;
        };
        let rem = clk.centis as f64 / 100.0;
        let prev_slot = match ply.color {
            Color::White => &mut prev_white,
            Color::Black => &mut prev_black,
        };
        let (move_time_secs, effort) = if let Some(prev) = *prev_slot {
            let dt = (prev - rem + inc).max(0.0);
            let eff_budget = (prev + inc * 25.0).max(1.0);
            let eff = (dt / eff_budget).clamp(0.0, 1.0);
            (Some(dt), Some(eff))
        } else {
            (None, None)
        };
        *prev_slot = Some(rem);
        map.insert(
            node,
            NodeClockStat {
                color: ply.color,
                remaining_secs: rem,
                move_time_secs,
                effort,
            },
        );
    }
    map
}

fn median_move_times_by_color(
    clock_map: &std::collections::HashMap<usize, NodeClockStat>,
) -> (Option<f64>, Option<f64>) {
    let mut w: Vec<f64> = Vec::new();
    let mut b: Vec<f64> = Vec::new();
    for stat in clock_map.values() {
        if let Some(dt) = stat.move_time_secs {
            match stat.color {
                Color::White => w.push(dt),
                Color::Black => b.push(dt),
            }
        }
    }
    let median = |mut v: Vec<f64>| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[v.len() / 2])
    };
    (median(w), median(b))
}

#[allow(clippy::too_many_arguments)]
fn classify_time_tag(
    quality: Quality,
    criticality: Option<f64>,
    win_drop: f64,
    win_before: Option<f64>,
    move_time_secs: Option<f64>,
    clock_remaining_secs: Option<f64>,
    clock_effort: Option<f64>,
    median_think: Option<f64>,
    trouble_threshold: f64,
) -> Option<TimeManagementTag> {
    let dt = move_time_secs?;
    let rem = clock_remaining_secs.unwrap_or(0.0);
    let eff = clock_effort.unwrap_or(0.0);
    let med = median_think.unwrap_or(10.0).max(2.0);

    // 1. Time trouble: mistake/blunder/inaccuracy under clock pressure.
    if rem <= trouble_threshold
        && (matches!(
            quality,
            Quality::Inaccuracy | Quality::Mistake | Quality::Blunder
        ) || win_drop >= 8.0)
    {
        return Some(TimeManagementTag::TimeTrouble);
    }

    // 2. Impulsive critical mistake ("Rushed"): high criticality or significant
    // error played much faster than median while ample clock remained.
    if rem > trouble_threshold * 2.0
        && (win_drop >= 10.0 || matches!(quality, Quality::Mistake | Quality::Blunder))
        && (criticality.is_some_and(|k| k >= 10.0) || win_drop >= 14.0)
        && dt <= (med * 0.45).max(4.0)
    {
        return Some(TimeManagementTag::Rushed);
    }

    // 3. Earned deep think ("Clutch"): high criticality, deep think, found best move.
    if quality == Quality::Best
        && criticality.is_some_and(|k| k >= 15.0)
        && (dt >= med * 1.8 || eff >= 0.06)
    {
        return Some(TimeManagementTag::Clutch);
    }

    // 4. Flat / already-decisive overthink ("Overthink"): low criticality or
    // lopsided position, yet burned >8% of remaining budget and >3x median.
    let flat_or_decided = criticality.is_some_and(|k| k < 2.5)
        || win_before.is_some_and(|wb| !(8.0..=92.0).contains(&wb));
    if flat_or_decided && eff >= 0.08 && dt >= (med * 3.0).max(15.0) {
        return Some(TimeManagementTag::Overthink);
    }

    None
}

/// Summarize a player's clock pacing across their clocked games (no engine needed).
pub fn summarize_player_clocks(games: &[Game], player_key: &str) -> Option<PlayerClockSummary> {
    let mut clocked_games = 0usize;
    let mut trouble_games = 0usize;
    let mut pct_10 = Vec::new();
    let mut pct_20 = Vec::new();
    let mut pct_30 = Vec::new();
    let mut pct_40 = Vec::new();
    let mut exit_moves: Vec<(u32, f64)> = Vec::new();

    for game in games.iter().take(250) {
        let is_white = crate::players::player_key(game.white()).as_deref() == Some(player_key);
        let is_black = crate::players::player_key(game.black()).as_deref() == Some(player_key);
        if !is_white && !is_black {
            continue;
        }
        let target_color = if is_white { Color::White } else { Color::Black };
        let (tc_base, inc) = parse_time_control_header(game.header("TimeControl"));
        let mut player_clocks: Vec<(u32, f64, Option<f64>)> = Vec::new();
        let mut prev_rem = tc_base;
        for node in game.mainline() {
            let Some(ply) = game.nodes[node].mv.as_ref() else {
                continue;
            };
            if ply.color != target_color {
                continue;
            }
            let Some(clk) = game.nodes[node].clock else {
                continue;
            };
            let rem = clk.centis as f64 / 100.0;
            let dt = prev_rem.map(|p| (p - rem + inc).max(0.0));
            prev_rem = Some(rem);
            player_clocks.push((ply.number, rem, dt));
        }
        if player_clocks.len() < 4 {
            continue;
        }
        clocked_games += 1;
        let initial = tc_base
            .or_else(|| player_clocks.first().map(|(_, r, _)| *r))
            .unwrap_or(0.0);
        let trouble_thresh = time_trouble_threshold_secs(Some(initial));
        if player_clocks.iter().any(|(_, r, _)| *r <= trouble_thresh) {
            trouble_games += 1;
        }
        if initial > 0.0 {
            for (target_mv, bucket) in [
                (10u32, &mut pct_10),
                (20u32, &mut pct_20),
                (30u32, &mut pct_30),
                (40u32, &mut pct_40),
            ] {
                if let Some((_, rem, _)) = player_clocks.iter().find(|(m, _, _)| *m == target_mv) {
                    bucket.push((rem / initial * 100.0).clamp(0.0, 150.0));
                }
            }
        }
        // Detect "Prep exit think spike": first move in 4..=25 where think time
        // exceeds 3.5x the early opening average and is at least 30s (or >=8% of initial).
        let min_spike = (initial * 0.06).clamp(15.0, 120.0);
        let mut early_sum = 0.0;
        let mut early_cnt = 0usize;
        for &(mv_no, _, dt_opt) in &player_clocks {
            let Some(dt) = dt_opt else {
                continue;
            };
            if (4..=25).contains(&mv_no) {
                let early_avg = if early_cnt > 0 {
                    (early_sum / early_cnt as f64).max(3.0)
                } else {
                    8.0
                };
                if dt >= min_spike && dt >= early_avg * 3.0 {
                    exit_moves.push((mv_no, dt));
                    break;
                }
            }
            if mv_no <= 10 {
                early_sum += dt;
                early_cnt += 1;
            }
        }
    }

    if clocked_games == 0 {
        return None;
    }

    let prep_exit_move = if !exit_moves.is_empty() {
        exit_moves.sort_by_key(|&(mv, _)| mv);
        let med_mv = exit_moves[exit_moves.len() / 2].0;
        let avg_dt = exit_moves.iter().map(|(_, dt)| *dt).sum::<f64>() / exit_moves.len() as f64;
        Some((med_mv, avg_dt))
    } else {
        None
    };

    Some(PlayerClockSummary {
        clocked_games,
        pct_at_move_10: mean(&pct_10),
        pct_at_move_20: mean(&pct_20),
        pct_at_move_30: mean(&pct_30),
        pct_at_move_40: mean(&pct_40),
        time_trouble_rate: (trouble_games as f64 / clocked_games as f64) * 100.0,
        prep_exit_move,
    })
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
            ..Default::default()
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
    fn cloud_white_pov_scores_are_converted_to_side_to_move() {
        let games = crate::domain::parse_pgn(b"[Event \"Cloud POV\"]\n\n1. e4 e5 *\n").unwrap();
        let mut review = ReviewState::new(
            &games[0],
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            0,
        );
        let black_turn_fen = review
            .positions
            .iter()
            .find(|position| position.turn == Color::Black)
            .unwrap()
            .fen
            .clone();
        assert!(review.seed_from_cloud(
            &black_turn_fen,
            Some(Score::Cp(300)), // Lichess reports White POV.
            Some("e7e5".to_string()),
            vec!["e7e5".to_string()],
            Some(Score::Cp(100)),
            Some(30),
        ));
        let black_position = review
            .positions
            .iter()
            .position(|position| position.fen == black_turn_fen)
            .unwrap();
        let evaluated = review.evaluated[black_position].as_ref().unwrap();
        assert_eq!(evaluated.eval, Some(Score::Cp(-300)));
        assert_eq!(evaluated.second_eval, Some(Score::Cp(-100)));
        assert_eq!(evaluated.white_cp, Some(300.0));
    }

    #[test]
    fn cloud_seed_of_last_position_keeps_review_running_for_deepening() {
        let games = crate::domain::parse_pgn(b"[Event \"Cloud deepening\"]\n\n1. e4 *\n").unwrap();
        let mut review = ReviewState::new(
            &games[0],
            ReviewConfig {
                limit: SearchLimit::Depth(8),
            },
            0,
        );
        review.enable_two_pass_deepening();
        let first_fen = review.positions[0].fen.clone();
        let fen = review.positions.last().unwrap().fen.clone();
        assert!(review.seed_from_cloud(
            &first_fen,
            Some(Score::Cp(100)),
            Some("e2e4".to_string()),
            vec!["e2e4".to_string()],
            None,
            Some(30),
        ));
        assert!(review.seed_from_cloud(
            &fen,
            Some(Score::Cp(20)),
            Some("e7e5".to_string()),
            vec!["e7e5".to_string()],
            None,
            Some(30),
        ));
        assert!(
            review.running,
            "advance_review must be able to start pass 2"
        );
        assert!(!review.complete());
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
            ..Default::default()
        });
        review.evaluated[1] = Some(Evaluated {
            eval: Some(Score::Cp(-10)),
            best: Some("e7e5".to_string()),
            white_cp: Some(10.0),
            ..Default::default()
        });
        review.evaluated[2] = Some(Evaluated {
            eval: Some(Score::Cp(30)),
            best: Some("g1f3".to_string()),
            white_cp: Some(30.0),
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn criticality_clock_tags_and_corpus_clock_pacing_work() {
        let pgn = br#"[Event "Classical"]
[White "Carlsen, Magnus"]
[Black "Nepomniachtchi, Ian"]
[TimeControl "5400+30"]

1. e4 {[%clk 1:30:25]} e5 {[%clk 1:30:20]} 2. Nf3 {[%clk 1:30:50]} Nc6 {[%clk 1:30:40]} 3. Bb5 {[%clk 1:31:15]} a6 {[%clk 1:30:55]} 4. Ba4 {[%clk 1:24:00]} Nf6 {[%clk 0:02:10]} *
"#;
        let games = crate::domain::parse_pgn(pgn).unwrap();
        let game = &games[0];
        let mut review = ReviewState::new(
            game,
            ReviewConfig {
                limit: SearchLimit::Depth(12),
            },
            0,
        );
        for i in 0..review.total() {
            review.evaluated[i] = Some(Evaluated {
                eval: Some(Score::Cp(35)),
                second_eval: Some(Score::Cp(-250)),
                best: Some("e2e4".to_string()),
                pv: vec!["e2e4".to_string(), "e7e5".to_string()],
                white_cp: Some(35.0),
                depth: Some(16),
                from_cache: false,
            });
        }
        let moves = review.moves(game);
        assert!(!moves.is_empty());
        assert!(moves[0].criticality.unwrap() > 15.0);
        assert!(moves[0].is_only_move_great);
        assert!(moves[0].move_time_secs.is_some());

        let summary = summarize_player_clocks(&games, "carlsen magnus").unwrap();
        assert_eq!(summary.clocked_games, 1);
        assert_eq!(summary.prep_exit_move.map(|(m, _)| m), Some(4));
    }
}
