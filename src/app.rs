use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    pos2, vec2,
};
use shakmaty::fen::Fen;
use shakmaty::san::SanPlus;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, EnPassantMode, Move, Position, Square};

use crate::cbh::CbhLibrary;
use crate::cloud;
use crate::domain::{self, Clock, Game, Token};
use crate::engine::{EngineEvent, EngineHandle, EngineInfo, EngineOptions, Score, SearchLimit};
use crate::library::{GameLibrary, PgnLibrary};
use crate::online::{self, ImportResult, OnlineSource};
use crate::players::{self, PlayerBook, PlayerDirectory};
use crate::position::{
    self, BuildOptions, BuildProgress, PositionIndex, PositionStats, RatingFilter,
};
use crate::review::{self, Quality, ReviewConfig, ReviewState};
use crate::scid::ScidLibrary;
use crate::settings::{CloudPrefs, EnginePrefs, PreparationPrefs};
use crate::ui::board::{self, BoardEvent, BoardState};
use crate::ui::fonts;
use crate::ui::pieces::PieceTextures;

const SURFACE: Color32 = Color32::from_rgb(0x22, 0x25, 0x28);
const VARIATION_BG: Color32 = Color32::from_rgb(0x1D, 0x20, 0x23);
const BORDER: Color32 = Color32::from_rgb(0x38, 0x3C, 0x40);
const GOLD: Color32 = Color32::from_rgb(0xE8, 0xC4, 0x6A);
const GOLD_SOFT: Color32 = Color32::from_rgb(0x4A, 0x3F, 0x24);
const COMMENT_TEXT: Color32 = Color32::from_rgb(0x9E, 0xB0, 0xA9);

struct PlayerBadge {
    name: String,
    rating: Option<String>,
    clock: Option<Clock>,
    color: Color,
}

struct BoardMeta {
    pos: Chess,
    last_move: Option<(Square, Square)>,
    top: PlayerBadge,
    bottom: PlayerBadge,
    show_clocks: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LeftTab {
    #[default]
    Games,
    Players,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RightTab {
    #[default]
    Moves,
    Tree,
    Engine,
    Review,
    References,
}

enum IndexMessage {
    Progress(BuildProgress),
    Done(Result<PositionIndex, String>),
}

enum PlayerBookMessage {
    Progress { done: usize, total: usize },
    Done(Result<PlayerBook, String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReferenceJob {
    path: PathBuf,
    /// Effective reference target after applying the configured offset.
    target: u32,
    band: u32,
    offset: i32,
}

impl ReferenceJob {
    fn rating_filter(&self) -> RatingFilter {
        RatingFilter::around(self.target, self.band)
    }

    fn label(&self) -> String {
        let offset = if self.offset == 0 {
            String::new()
        } else {
            format!(" · offset {:+}", self.offset)
        };
        let filter = self.rating_filter();
        format!("{}–{} Elo{offset}", filter.min, filter.max)
    }
}

struct ReferenceIndex {
    index: PositionIndex,
    job: ReferenceJob,
}

enum ReferenceMessage {
    Progress(BuildProgress),
    Done(ReferenceJob, Result<PositionIndex, String>),
}

enum OnlineMessage {
    Progress(String),
    Done(Result<ImportResult, String>),
}

enum CloudMessage {
    Done(cloud::Snapshot),
}

#[derive(Clone, Debug)]
struct PrepSession {
    player_key: String,
    player_name: String,
    our_color: Color,
    previous_game: Option<usize>,
}

impl PrepSession {
    fn opponent_color(&self) -> Color {
        self.our_color.other()
    }
}

/// One row of the opponent-prediction table, combining player likelihood with
/// the normal position-tree statistics for the same candidate move.
#[derive(Clone, Debug)]
struct PrepPredictionRow {
    mv: Move,
    san: String,
    probability: f64,
    probability_low: f64,
    probability_high: f64,
    player_games: u32,
    recent_games: u32,
    prior_games: u32,
    last_date: u32,
    trend_pp: Option<f64>,
    tree: Option<position::MoveStat>,
}

struct PrepPredictionData {
    rows: Vec<PrepPredictionRow>,
    stats: Option<players::PlayerPositionStats>,
    prediction: players::Prediction,
    warning: Option<String>,
}

/// One move by the prepared player in the line currently on the board.
#[derive(Clone, Debug)]
struct PrepLineRow {
    label: String,
    date: u32,
    tree_score: Option<f32>,
    likelihood: Option<f64>,
    likelihood_low: Option<f64>,
    likelihood_high: Option<f64>,
}

/// One clickable recent-game row.
#[derive(Clone, Debug)]
struct PrepRecentRow {
    index: usize,
    title: String,
    subtitle: String,
}

/// Read-only recent-game view. The preparation game and cursor are kept intact
/// so closing the preview can restore them exactly.
#[derive(Clone, Debug)]
struct RecentPreview {
    return_game: Game,
    return_node: usize,
    return_tab: RightTab,
}

pub struct App {
    library: Option<Arc<dyn GameLibrary>>,
    current: Option<Game>,
    /// Games edited during this session, kept by index so switching away and
    /// back does not discard new moves or variations.
    edited: std::collections::HashMap<usize, Game>,
    dirty: bool,
    game_idx: usize,
    node: usize,
    orientation: Color,
    show_clocks: bool,
    selected: Option<Square>,
    targets: Vec<Square>,
    drag_from: Option<Square>,
    status: String,
    index: Option<PositionIndex>,
    index_rx: Option<mpsc::Receiver<IndexMessage>>,
    index_job: Option<PathBuf>,
    index_status: String,
    left_tab: LeftTab,
    right_tab: RightTab,
    players: PlayerDirectory,
    player_filter: String,
    selected_player: Option<usize>,
    player_book: Option<PlayerBook>,
    player_book_rx: Option<mpsc::Receiver<PlayerBookMessage>>,
    player_book_job: Option<String>,
    player_book_status: String,
    prep: Option<PrepSession>,
    prep_recent_limit: usize,
    recent_preview: Option<RecentPreview>,
    reference_path: String,
    reference_enabled: bool,
    reference_band: u32,
    reference_offset: i32,
    reference: Option<ReferenceIndex>,
    reference_rx: Option<mpsc::Receiver<ReferenceMessage>>,
    reference_job: Option<ReferenceJob>,
    reference_status: String,
    /// Move whose annotation editor is open. Clicking an annotation or using
    /// the move context menu enters this mode.
    annotation_editor: Option<usize>,
    /// Request keyboard focus on the annotation comment box once.
    annotation_focus: bool,
    online_open: bool,
    online_source: OnlineSource,
    online_username: String,
    online_max: usize,
    online_status: String,
    online_rx: Option<mpsc::Receiver<OnlineMessage>>,
    cloud: Option<cloud::Snapshot>,
    cloud_cache: BTreeMap<String, cloud::Snapshot>,
    cloud_rx: Option<mpsc::Receiver<CloudMessage>>,
    cloud_job: Option<String>,
    cloud_pending: Option<(String, Instant)>,
    cloud_status: String,
    lichess_token: Option<String>,
    engine: Option<EngineHandle>,
    engine_status: String,
    engine_name: String,
    engine_ready: bool,
    engine_auto: bool,
    engine_multipv: usize,
    engine_depth: u32,
    engine_time_ms: u64,
    engine_use_time: bool,
    engine_request: u64,
    engine_fen: Option<String>,
    engine_infos: BTreeMap<usize, EngineInfo>,
    engine_bestmove: Option<String>,
    engine_searching: bool,
    engine_path: String,
    engine_threads: u32,
    engine_hash_mb: u32,
    review_engine: Option<EngineHandle>,
    review: Option<ReviewState>,
    review_status: String,
    review_depth: u32,
    review_time_ms: u64,
    review_use_time: bool,
    path_input: String,
    filter: String,
    show_about: bool,
    textures: PieceTextures,
    ctx: egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let start = std::env::args_os().nth(1).map(std::path::PathBuf::from);
        Self::with_file(cc, start)
    }

    pub fn with_file(cc: &eframe::CreationContext<'_>, start: Option<std::path::PathBuf>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(0x1B, 0x1D, 0x1F);
        visuals.window_fill = Color32::from_rgb(0x22, 0x24, 0x27);
        visuals.extreme_bg_color = Color32::from_rgb(0x14, 0x15, 0x17);
        cc.egui_ctx.set_visuals(visuals);
        fonts::install(&cc.egui_ctx);
        let engine_prefs = EnginePrefs::load();
        let preparation_prefs = PreparationPrefs::load();
        let cloud_prefs = CloudPrefs::load();
        let reference_path = preparation_prefs
            .reference_path
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| {
                std::env::var_os("BRETT_REFERENCE")
                    .map(PathBuf::from)
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.display().to_string())
            })
            .unwrap_or_default();
        let reference_status = if !preparation_prefs.reference_enabled {
            "Reference database disabled — using the current database prior (may be skewed)."
                .to_string()
        } else if reference_path.is_empty() {
            "No reference database configured — using the current database prior (may be skewed)."
                .to_string()
        } else {
            "Reference database configured; it will be indexed for the prepared player's rating."
                .to_string()
        };

        let mut app = Self {
            library: None,
            current: None,
            edited: std::collections::HashMap::new(),
            dirty: false,
            game_idx: 0,
            node: 0,
            orientation: Color::White,
            show_clocks: false,
            selected: None,
            targets: Vec::new(),
            drag_from: None,
            status: "Open a PGN, SCID, or classic ChessBase database, or drop one onto the window."
                .to_string(),
            index: None,
            index_rx: None,
            index_job: None,
            index_status: "No position index yet.".to_string(),
            left_tab: LeftTab::Games,
            right_tab: RightTab::Moves,
            players: PlayerDirectory::default(),
            player_filter: String::new(),
            selected_player: None,
            player_book: None,
            player_book_rx: None,
            player_book_job: None,
            player_book_status: "Select a player to build their preparation book.".to_string(),
            prep: None,
            prep_recent_limit: 10,
            recent_preview: None,
            reference_path,
            reference_enabled: preparation_prefs.reference_enabled,
            reference_band: preparation_prefs.rating_band,
            reference_offset: preparation_prefs.rating_offset,
            reference: None,
            reference_rx: None,
            reference_job: None,
            reference_status,
            annotation_editor: None,
            annotation_focus: false,
            online_open: false,
            online_source: OnlineSource::Lichess,
            online_username: String::new(),
            online_max: 500,
            online_status: String::new(),
            online_rx: None,
            cloud: None,
            cloud_cache: BTreeMap::new(),
            cloud_rx: None,
            cloud_job: None,
            cloud_pending: None,
            cloud_status: "Open a game and select References.".to_string(),
            lichess_token: cloud_prefs.lichess_token,
            engine: None,
            engine_status: "Engine not started.".to_string(),
            engine_name: String::new(),
            engine_ready: false,
            engine_auto: true,
            engine_multipv: 3,
            engine_depth: 18,
            engine_time_ms: 1_500,
            engine_use_time: false,
            engine_request: 0,
            engine_fen: None,
            engine_infos: BTreeMap::new(),
            engine_bestmove: None,
            engine_searching: false,
            engine_path: engine_prefs.resolved_path().display().to_string(),
            engine_threads: engine_prefs.threads,
            engine_hash_mb: engine_prefs.hash_mb,
            review_engine: None,
            review: None,
            review_status: "No game review yet.".to_string(),
            review_depth: 14,
            review_time_ms: 500,
            review_use_time: true,
            path_input: String::new(),
            filter: String::new(),
            show_about: false,
            textures: PieceTextures::load(&cc.egui_ctx),
            ctx: cc.egui_ctx.clone(),
        };

        if let Some(path) = start {
            app.load_file(&path);
        }
        app
    }

    fn game(&self) -> Option<&Game> {
        self.current.as_ref()
    }

    fn goto(&mut self, node: usize) {
        let changed = self.node != node;
        if self
            .annotation_editor
            .is_some_and(|editing| editing != node)
        {
            self.annotation_editor = None;
            self.annotation_focus = false;
        }
        self.node = node;
        self.selected = None;
        self.targets.clear();
        self.drag_from = None;
        if changed {
            self.clear_engine_results();
        }
    }

    fn next_move(&mut self) {
        if let Some(g) = self.game()
            && let Some(&child) = g.nodes[self.node].children.first()
        {
            self.goto(child);
        }
    }

    fn prev_move(&mut self) {
        if let Some(g) = self.game()
            && let Some(parent) = g.nodes[self.node].parent
        {
            self.goto(parent);
        }
    }

    fn next_variation(&mut self) {
        if let Some(g) = self.game()
            && let Some(p) = g.nodes[self.node].parent
        {
            let siblings = &g.nodes[p].children;
            if let Some(pos) = siblings.iter().position(|&c| c == self.node)
                && pos + 1 < siblings.len()
            {
                let next = siblings[pos + 1];
                self.goto(next);
            }
        }
    }

    fn go_end(&mut self) {
        if let Some(g) = self.game()
            && let Some(&last) = g.mainline().last()
        {
            self.goto(last);
        }
    }

    fn load_file(&mut self, path: &std::path::Path) {
        let opened = open_library(path);
        match opened {
            Ok(library) => {
                let count = library.len();
                let indexed_in = library.indexed_in();
                self.players = PlayerDirectory::build(library.as_ref());
                self.library = Some(library);
                self.edited.clear();
                self.dirty = false;
                self.game_idx = 0;
                self.current = None;
                self.clear_engine_results();
                self.goto(0);
                self.left_tab = LeftTab::Games;
                self.right_tab = RightTab::Moves;
                self.selected_player = None;
                self.player_filter.clear();
                self.player_book = None;
                self.player_book_rx = None;
                self.player_book_job = None;
                self.player_book_status =
                    "Select a player to build their preparation book.".to_string();
                self.prep = None;
                self.recent_preview = None;
                self.cloud = None;
                self.cloud_cache.clear();
                self.cloud_rx = None;
                self.cloud_job = None;
                self.cloud_pending = None;
                self.cloud_status = "Open a game and select References.".to_string();
                self.annotation_editor = None;
                self.annotation_focus = false;
                self.index_status = format!(
                    "{} from {} · indexed in {:.2}s",
                    games_label(count),
                    path.display(),
                    indexed_in.as_secs_f64()
                );
                self.status = self.index_status.clone();
                self.path_input = path.display().to_string();
                self.select_game(0);
                self.start_index();
            }
            Err(err) => self.status = format!("Could not read {}: {err}", path.display()),
        }
    }

    /// Load a cached position index, or build one in the background.
    fn start_index(&mut self) {
        let Some(library) = self.library.clone() else {
            return;
        };
        self.index = None;
        self.index_rx = None;
        self.index_job = Some(library.path().to_path_buf());
        self.index_status = "Checking the position index…".to_string();

        match PositionIndex::open_cached_for(library.as_ref()) {
            Ok(Some(index)) => {
                self.index_status = index_summary(&index);
                self.index = Some(index);
                self.index_job = None;
                return;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("could not open cached position index: {err:#}");
            }
        }

        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = PositionIndex::build_cached_for(
                library,
                BuildOptions::default(),
                move |progress| {
                    let _ = progress_tx.send(IndexMessage::Progress(progress));
                    progress_ctx.request_repaint();
                },
            )
            .map_err(|err| format!("{err:#}"));
            let _ = tx.send(IndexMessage::Done(result));
            ctx.request_repaint();
        });
        self.index_rx = Some(rx);
        self.index_status = "Indexing positions…".to_string();
    }

    fn poll_index(&mut self) {
        let Some(rx) = self.index_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(IndexMessage::Progress(progress)) => {
                    self.index_status = format!(
                        "Indexing positions… {} games, {} moves ({:.1}s)",
                        progress.games,
                        progress.edges,
                        progress.elapsed.as_secs_f64()
                    );
                }
                Ok(IndexMessage::Done(Ok(index))) => {
                    self.index_status = index_summary(&index);
                    self.index = Some(index);
                    self.index_job = None;
                    keep = false;
                    break;
                }
                Ok(IndexMessage::Done(Err(err))) => {
                    self.index_status = format!("Position index failed: {err}");
                    self.index_job = None;
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.index.is_none() {
                        self.index_status =
                            "Position index build stopped unexpectedly.".to_string();
                    }
                    self.index_job = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.index_rx = Some(rx);
        }
    }

    fn reference_wanted(&self) -> bool {
        self.reference_enabled && !self.reference_path.trim().is_empty()
    }

    fn reference_target(&self, player: &players::PlayerSummary) -> Option<u32> {
        let base = player.elo_avg()?.round() as i64;
        Some((base + i64::from(self.reference_offset)).clamp(50, 4_000) as u32)
    }

    fn save_reference_prefs(&mut self) {
        let prefs = PreparationPrefs {
            reference_path: {
                let path = self.reference_path.trim();
                (!path.is_empty()).then(|| PathBuf::from(path))
            },
            reference_enabled: self.reference_enabled,
            rating_band: self.reference_band.clamp(25, 1_000),
            rating_offset: self.reference_offset.clamp(-1_000, 1_000),
        };
        if let Err(err) = prefs.save() {
            self.reference_status = format!("Could not save reference settings: {err:#}");
        }
    }

    fn ensure_reference_for_player(&mut self, player: &players::PlayerSummary) {
        if !self.reference_enabled {
            self.reference = None;
            self.reference_rx = None;
            self.reference_job = None;
            self.reference_status =
                "Reference database disabled — using the current database prior (may be skewed)."
                    .to_string();
            return;
        }
        let path = self.reference_path.trim();
        if path.is_empty() {
            self.reference = None;
            self.reference_rx = None;
            self.reference_job = None;
            self.reference_status =
                "No reference database configured — using the current database prior (may be skewed)."
                    .to_string();
            return;
        }
        let Some(target) = self.reference_target(player) else {
            self.reference = None;
            self.reference_rx = None;
            self.reference_job = None;
            self.reference_status = format!(
                "{} has no usable rating; cannot build a rating-filtered reference window. Using the current database prior; it may be skewed.",
                player.name
            );
            return;
        };
        let job = ReferenceJob {
            path: PathBuf::from(path),
            target,
            band: self.reference_band.clamp(25, 1_000),
            offset: self.reference_offset,
        };
        if self
            .reference
            .as_ref()
            .is_some_and(|reference| reference.job == job)
        {
            self.reference_status = self.reference_ready_status();
            return;
        }
        if self.reference_job.as_ref() == Some(&job) {
            return;
        }
        self.start_reference(job);
    }

    fn reference_ready_status(&self) -> String {
        let Some(reference) = &self.reference else {
            return "Reference index is not ready.".to_string();
        };
        format!(
            "Reference prior ready: {} · {} games · {} positions",
            reference.job.label(),
            games_label(reference.index.game_count()),
            positions_label(reference.index.position_count())
        )
    }

    fn start_reference(&mut self, job: ReferenceJob) {
        self.reference = None;
        self.reference_job = Some(job.clone());
        self.reference_status = format!(
            "Indexing reference {} for {} — using the current database prior meanwhile…",
            job.path.display(),
            job.label()
        );
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let thread_job = job.clone();
            let result = open_library(&thread_job.path)
                .and_then(|library| {
                    let filter = Some(thread_job.rating_filter());
                    let cache = position::reference_cache_path(&thread_job.path, filter);
                    if let Some(index) =
                        PositionIndex::open_cached_for_at(library.as_ref(), &cache)?
                    {
                        return Ok(index);
                    }
                    PositionIndex::build_cached_for_at(
                        library,
                        &cache,
                        BuildOptions {
                            max_plies: u32::MAX,
                            rating_filter: filter,
                        },
                        move |progress| {
                            let _ = progress_tx.send(ReferenceMessage::Progress(progress));
                            progress_ctx.request_repaint();
                        },
                    )
                })
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(ReferenceMessage::Done(thread_job, result));
            ctx.request_repaint();
        });
        self.reference_rx = Some(rx);
    }

    fn poll_reference(&mut self) {
        let Some(rx) = self.reference_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(ReferenceMessage::Progress(progress)) => {
                    if let Some(job) = &self.reference_job {
                        self.reference_status = format!(
                            "Indexing reference for {}… {} included, {} moves ({:.1}s)",
                            job.label(),
                            games_label(progress.games),
                            moves_label(progress.edges),
                            progress.elapsed.as_secs_f64()
                        );
                    }
                }
                Ok(ReferenceMessage::Done(job, Ok(index))) => {
                    if self.reference_job.as_ref() == Some(&job) {
                        self.reference_job = None;
                        self.reference = Some(ReferenceIndex { index, job });
                        self.reference_status = self.reference_ready_status();
                    }
                    keep = false;
                    break;
                }
                Ok(ReferenceMessage::Done(job, Err(err))) => {
                    if self.reference_job.as_ref() == Some(&job) {
                        self.reference_job = None;
                        self.reference = None;
                        self.reference_status = format!(
                            "Reference index failed: {err} — using the current database prior (may be skewed)."
                        );
                    }
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.reference_job.is_some() {
                        self.reference_job = None;
                        self.reference_status =
                            "Reference index build stopped unexpectedly — using the current database prior; it may be skewed."
                                .to_string();
                    }
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.reference_rx = Some(rx);
        }
    }

    fn prior_for_position(&self, key: u64) -> (Vec<position::MoveStat>, players::PriorSource) {
        if self.reference_wanted()
            && let Some(reference) = &self.reference
        {
            let moves = reference.index.moves(key);
            if !moves.is_empty() {
                return (moves, players::PriorSource::Reference);
            }
        }
        let moves = self
            .index
            .as_ref()
            .map(|index| index.moves(key))
            .unwrap_or_default();
        (moves, players::PriorSource::CurrentDatabase)
    }

    fn prior_warning(&self, source: players::PriorSource) -> Option<String> {
        if source == players::PriorSource::Reference {
            return None;
        }
        let warning = if !self.reference_enabled {
            "Reference database disabled — using the current database prior; it may be skewed."
        } else if self.reference_path.trim().is_empty() {
            "No reference database configured — using the current database prior; it may be skewed."
        } else if self.reference_job.is_some() {
            "Reference index is still building — using the current database prior meanwhile."
        } else if self
            .reference
            .as_ref()
            .is_some_and(|reference| reference.index.game_count() == 0)
        {
            "Reference window contains no rated games — using the current database prior; it may be skewed."
        } else if self.reference.is_some() {
            "Reference has no games at this position — using the current database prior; it may be skewed."
        } else {
            "Reference prior unavailable — using the current database prior; it may be skewed."
        };
        Some(warning.to_string())
    }

    fn current_fen(&self) -> Option<String> {
        let game = self.current.as_ref()?;
        let node = game.nodes.get(self.node)?;
        Some(Fen::from_position(&node.pos, EnPassantMode::Legal).to_string())
    }

    fn engine_limit(&self) -> SearchLimit {
        if self.engine_use_time {
            SearchLimit::MoveTime(Duration::from_millis(self.engine_time_ms))
        } else {
            SearchLimit::Depth(self.engine_depth)
        }
    }

    fn engine_path(&self) -> PathBuf {
        let trimmed = self.engine_path.trim();
        if trimmed.is_empty() {
            crate::engine::default_engine_path()
        } else {
            PathBuf::from(trimmed)
        }
    }

    fn engine_options(&self) -> EngineOptions {
        EngineOptions {
            threads: self.engine_threads.clamp(1, 256),
            hash_mb: self.engine_hash_mb.clamp(1, 32_768),
        }
    }

    fn engine_settings_pending(&self) -> bool {
        self.engine.as_ref().is_some_and(|engine| {
            engine.path() != self.engine_path() || engine.options() != self.engine_options()
        })
    }

    fn reset_engine_events(&mut self) {
        self.engine_request = 0;
        self.engine_fen = None;
        self.engine_infos.clear();
        self.engine_bestmove = None;
        self.engine_searching = false;
        self.engine_ready = false;
    }

    /// Start the engine lazily with the current executable and resource
    /// settings. If the settings changed, the old process is replaced.
    fn ensure_engine(&mut self) -> bool {
        let path = self.engine_path();
        let options = self.engine_options();
        if let Some(engine) = &self.engine {
            if engine.path() == path && engine.options() == options {
                return true;
            }
            self.engine = None;
            self.reset_engine_events();
            self.engine_name.clear();
        }
        match EngineHandle::spawn_with_options(&path, options) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.reset_engine_events();
                self.engine_name.clear();
                self.engine_status = format!("Starting {}…", path.display());
                true
            }
            Err(err) => {
                self.engine_status = format!("Engine unavailable: {err:#}");
                false
            }
        }
    }

    /// Save the current engine selection and replace both engine processes.
    fn apply_engine_settings(&mut self) {
        self.engine_path = self.engine_path().display().to_string();
        self.engine_threads = self.engine_options().threads;
        self.engine_hash_mb = self.engine_options().hash_mb;
        let was_running = self.engine.is_some();
        if let Some(engine) = &self.engine {
            engine.stop();
        }
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        self.engine = None;
        self.review_engine = None;
        self.reset_engine_events();
        self.engine_name.clear();

        let review_was_running = self.review.as_ref().is_some_and(|review| review.running);
        if review_was_running {
            self.cancel_review();
            self.review_status = "Review canceled: engine settings changed.".to_string();
        }

        let prefs = EnginePrefs {
            path: Some(self.engine_path()),
            threads: self.engine_options().threads,
            hash_mb: self.engine_options().hash_mb,
        };
        if let Err(err) = prefs.save() {
            self.status = format!("Could not save engine settings: {err:#}");
        } else if let Some(path) = crate::settings::config_path() {
            self.status = format!("Engine settings saved to {}.", path.display());
        }

        if was_running || self.right_tab == RightTab::Engine {
            self.ensure_engine();
        } else {
            self.engine_status = "Engine settings saved.".to_string();
        }
    }

    fn clear_engine_results(&mut self) {
        if let Some(engine) = &self.engine {
            engine.stop();
        }
        self.engine_request = 0;
        self.engine_fen = None;
        self.engine_infos.clear();
        self.engine_bestmove = None;
        self.engine_searching = false;
    }

    fn request_engine_analysis(&mut self) {
        if !self.ensure_engine() {
            return;
        }
        let Some(fen) = self.current_fen() else {
            return;
        };
        let multipv = self.engine_multipv;
        let limit = self.engine_limit();
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let request = engine.analyze(fen.clone(), multipv, limit);
        self.engine_request = request;
        self.engine_fen = Some(fen);
        self.engine_infos.clear();
        self.engine_bestmove = None;
        self.engine_searching = true;
        self.engine_status = match limit {
            SearchLimit::Depth(depth) => format!("Searching to depth {depth}…"),
            SearchLimit::MoveTime(time) => {
                format!("Searching for {} ms…", time.as_millis())
            }
        };
    }

    fn maybe_engine_analysis(&mut self) {
        if self.right_tab != RightTab::Engine {
            return;
        }
        if self.engine.is_none() {
            if self.engine_status.starts_with("Engine unavailable") {
                return;
            }
            self.ensure_engine();
            return;
        }
        if !self.engine_ready || !self.engine_auto {
            return;
        }
        let Some(fen) = self.current_fen() else {
            return;
        };
        if self.engine_fen.as_deref() != Some(fen.as_str()) {
            self.request_engine_analysis();
        }
    }

    fn poll_engine(&mut self) {
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let mut events = Vec::new();
        while let Some(event) = engine.try_recv() {
            events.push(event);
        }
        for event in events {
            match event {
                EngineEvent::Name(name) => self.engine_name = name,
                EngineEvent::Ready => {
                    self.engine_ready = true;
                    self.engine_status = if self.engine_name.is_empty() {
                        "Engine ready.".to_string()
                    } else {
                        format!("{} ready.", self.engine_name)
                    };
                }
                EngineEvent::Info { request, info } => {
                    if request == self.engine_request {
                        self.engine_infos.insert(info.multipv, info);
                    }
                }
                EngineEvent::BestMove { request, best, .. } => {
                    if request == self.engine_request {
                        self.engine_searching = false;
                        self.engine_bestmove = Some(best);
                        self.engine_status = "Analysis complete.".to_string();
                    }
                }
                EngineEvent::Error(err) => {
                    self.engine_status = format!("Engine error: {err}");
                }
                EngineEvent::Exited => {
                    self.engine = None;
                    self.engine_ready = false;
                    self.engine_searching = false;
                    self.engine_infos.clear();
                    self.engine_bestmove = None;
                    self.engine_status = "Engine exited.".to_string();
                }
            }
        }
    }

    fn review_limit(&self) -> SearchLimit {
        if self.review_use_time {
            SearchLimit::MoveTime(Duration::from_millis(self.review_time_ms))
        } else {
            SearchLimit::Depth(self.review_depth)
        }
    }

    fn ensure_review_engine(&mut self) -> bool {
        let path = self.engine_path();
        let options = self.engine_options();
        if let Some(engine) = &self.review_engine {
            if engine.path() == path && engine.options() == options {
                return true;
            }
            self.review_engine = None;
        }
        match EngineHandle::spawn_with_options(&path, options) {
            Ok(engine) => {
                self.review_engine = Some(engine);
                true
            }
            Err(err) => {
                self.review_status = format!("Review engine unavailable: {err:#}");
                false
            }
        }
    }

    fn start_review(&mut self) {
        let Some(game) = self.current.as_ref() else {
            self.review_status = "Open a game before starting a review.".to_string();
            return;
        };
        let config = ReviewConfig {
            limit: self.review_limit(),
        };
        let mut review = ReviewState::new(game, config);
        if !self.ensure_review_engine() {
            return;
        }
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        let Some(engine) = self.review_engine.as_mut() else {
            return;
        };
        if !review.request_next(engine) {
            self.review_status = "This game has no positions to review.".to_string();
            return;
        }
        let total = review.total();
        self.review = Some(review);
        self.review_status = format!("Reviewing {total} positions…");
    }

    fn cancel_review(&mut self) {
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        if let Some(review) = &mut self.review {
            review.cancel();
        }
        self.review_status = "Review stopped.".to_string();
    }

    fn clear_review(&mut self) {
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        self.review = None;
        self.review_status = "No game review yet.".to_string();
    }

    fn poll_review(&mut self) {
        let Some(engine) = self.review_engine.as_ref() else {
            return;
        };
        let mut events = Vec::new();
        while let Some(event) = engine.try_recv() {
            events.push(event);
        }
        for event in events {
            match event {
                EngineEvent::Info { request, info } => {
                    if let Some(review) = &mut self.review {
                        review.note_info(request, info);
                    }
                }
                EngineEvent::BestMove { request, best, .. } => {
                    let mut next_request = false;
                    let mut status = None;
                    if let Some(review) = &mut self.review
                        && review.finish_search(request, &best)
                    {
                        next_request = review.running;
                        let (done, total) = review.progress();
                        status = Some(if next_request {
                            format!("Reviewing: {done}/{total} positions…")
                        } else {
                            format!("Review complete: {done}/{total} positions.")
                        });
                    }
                    if let Some(status) = status {
                        self.review_status = status;
                    }
                    if next_request {
                        let mut failed = false;
                        if let (Some(engine), Some(review)) =
                            (self.review_engine.as_mut(), self.review.as_mut())
                            && !review.request_next(engine)
                        {
                            review.running = false;
                            failed = true;
                        }
                        if failed {
                            self.review_status =
                                "Review stopped: the engine request failed.".to_string();
                        }
                    }
                }
                EngineEvent::Error(err) => {
                    self.review_status = format!("Review engine error: {err}");
                    if let Some(review) = &mut self.review {
                        review.cancel();
                    }
                }
                EngineEvent::Exited => {
                    self.review_engine = None;
                    if let Some(review) = &mut self.review
                        && review.running
                    {
                        review.cancel();
                        self.review_status = "Review engine exited.".to_string();
                    }
                }
                EngineEvent::Name(_) | EngineEvent::Ready => {}
            }
        }
    }

    fn select_player(&mut self, index: usize) {
        let Some(player) = self.players.get(index).cloned() else {
            return;
        };
        if self
            .prep
            .as_ref()
            .is_some_and(|prep| prep.player_key != player.key)
        {
            self.exit_prep();
        }
        self.left_tab = LeftTab::Players;
        self.selected_player = Some(index);
        self.start_player_book(index);
    }

    /// Build a player's position book in the background.
    fn start_player_book(&mut self, index: usize) {
        let Some(player) = self.players.get(index).cloned() else {
            return;
        };
        if self
            .player_book
            .as_ref()
            .is_some_and(|book| book.player_key() == player.key)
        {
            return;
        }
        if self.player_book_job.as_deref() == Some(player.key.as_str())
            && self.player_book_rx.is_some()
        {
            return;
        }
        let Some(library) = self.library.clone() else {
            return;
        };

        self.player_book = None;
        self.player_book_job = Some(player.key.clone());
        self.player_book_status = format!(
            "Indexing {} games for {}…",
            games_label(player.game_indices.len()),
            player.name
        );
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = PlayerBook::build(
                library.as_ref(),
                &player,
                players::PredictionConfig::default(),
                players::today(),
                move |done, total| {
                    let _ = progress_tx.send(PlayerBookMessage::Progress { done, total });
                    progress_ctx.request_repaint();
                },
            )
            .map_err(|err| format!("{err:#}"));
            let _ = tx.send(PlayerBookMessage::Done(result));
            ctx.request_repaint();
        });
        self.player_book_rx = Some(rx);
    }

    fn poll_player_book(&mut self) {
        let Some(rx) = self.player_book_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(PlayerBookMessage::Progress { done, total }) => {
                    self.player_book_status = format!("Indexing player games… {done}/{total}");
                }
                Ok(PlayerBookMessage::Done(Ok(book))) => {
                    if self.player_book_job.as_deref() == Some(book.player_key()) {
                        self.player_book_status = format!(
                            "{} games indexed for {} · ready for prediction",
                            book.games(),
                            book.player_name()
                        );
                        self.player_book = Some(book);
                        self.player_book_job = None;
                    }
                    keep = false;
                    break;
                }
                Ok(PlayerBookMessage::Done(Err(err))) => {
                    self.player_book_status = format!("Player index failed: {err}");
                    self.player_book_job = None;
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.player_book.is_none() {
                        self.player_book_status = "Player index stopped unexpectedly.".to_string();
                    }
                    self.player_book_job = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.player_book_rx = Some(rx);
        }
    }

    fn start_prep(&mut self, as_white: bool) {
        let Some(index) = self.selected_player else {
            return;
        };
        let Some(player) = self.players.get(index).cloned() else {
            return;
        };

        self.ensure_reference_for_player(&player);
        let our_color = if as_white { Color::White } else { Color::Black };
        let previous_game = if let Some(prep) = &self.prep {
            prep.previous_game
        } else {
            let previous = (self
                .library
                .as_ref()
                .is_some_and(|l| self.game_idx < l.len()))
            .then_some(self.game_idx);
            if self.dirty
                && let Some(game) = self.current.take()
            {
                self.edited.insert(self.game_idx, game);
            }
            previous
        };

        let (white, black) = if as_white {
            ("You".to_string(), player.name.clone())
        } else {
            (player.name.clone(), "You".to_string())
        };
        let game = Game {
            headers: vec![
                ("Event".to_string(), "Preparation".to_string()),
                ("White".to_string(), white),
                ("Black".to_string(), black),
                ("Date".to_string(), pgn_date(players::today())),
                ("Result".to_string(), "*".to_string()),
            ],
            ..Game::default()
        };

        self.clear_review();
        self.recent_preview = None;
        self.prep = Some(PrepSession {
            player_key: player.key.clone(),
            player_name: player.name.clone(),
            our_color,
            previous_game,
        });
        self.annotation_editor = None;
        self.annotation_focus = false;
        self.current = Some(game);
        self.game_idx = usize::MAX;
        self.dirty = false;
        self.right_tab = RightTab::Tree;
        self.goto(0);
        self.status = format!(
            "Preparing against {} as {}",
            player.name,
            color_name(our_color)
        );
        self.start_player_book(index);
    }

    fn exit_prep(&mut self) {
        let Some(prep) = self.prep.take() else {
            return;
        };
        self.recent_preview = None;
        self.dirty = false;
        self.current = None;
        self.game_idx = usize::MAX;
        self.annotation_editor = None;
        self.annotation_focus = false;
        let target = prep.previous_game.or_else(|| {
            self.library
                .as_ref()
                .filter(|library| !library.is_empty())
                .map(|_| 0)
        });
        if let Some(index) = target {
            self.select_game(index);
        } else {
            self.status = "Preparation closed.".to_string();
        }
    }

    fn start_online_import(&mut self) {
        if self.online_rx.is_some() {
            return;
        }
        let source = self.online_source;
        let username = self.online_username.trim().to_string();
        let max = self.online_max;
        self.online_status = format!("Preparing {} import…", source.label());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = online::import(source, &username, max, move |message| {
                let _ = progress_tx.send(OnlineMessage::Progress(message));
                progress_ctx.request_repaint();
            })
            .map_err(|err| format!("{err:#}"));
            let _ = tx.send(OnlineMessage::Done(result));
            ctx.request_repaint();
        });
        self.online_rx = Some(rx);
    }

    fn poll_online(&mut self) {
        let Some(rx) = self.online_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(OnlineMessage::Progress(message)) => self.online_status = message,
                Ok(OnlineMessage::Done(Ok(result))) => {
                    let username = result.username.clone();
                    let games = result.games;
                    self.online_status = match games {
                        Some(count) => format!(
                            "Imported {} from {}.",
                            games_label(count),
                            result.source.label()
                        ),
                        None => format!("Imported {} games.", result.source.label()),
                    };
                    self.online_open = false;
                    self.load_file(&result.path);
                    if let Some(key) = players::player_key(&username)
                        && let Some(index) = self.players.find_key(&key)
                    {
                        self.select_player(index);
                    }
                    keep = false;
                    break;
                }
                Ok(OnlineMessage::Done(Err(err))) => {
                    self.online_status = format!("Online import failed: {err}");
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.online_status = "Online import stopped unexpectedly.".to_string();
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.online_rx = Some(rx);
        }
    }

    fn poll_cloud(&mut self) {
        let Some(rx) = self.cloud_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(CloudMessage::Done(snapshot)) => {
                let fen = snapshot.fen.clone();
                if let Some(current) = self.current_fen()
                    && current == fen
                {
                    self.cloud = Some(snapshot.clone());
                    self.cloud_status = if snapshot.has_any_data() {
                        "References loaded.".to_string()
                    } else {
                        "No external references found for this position.".to_string()
                    };
                }
                self.cloud_job = None;
                self.cloud_cache.insert(fen, snapshot);
                if self.cloud_cache.len() > 128
                    && let Some(key) = self.cloud_cache.keys().next().cloned()
                {
                    self.cloud_cache.remove(&key);
                }
            }
            Err(TryRecvError::Empty) => {
                self.cloud_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.cloud_job = None;
                self.cloud_status =
                    "Reference query stopped unexpectedly. Press Refresh to retry.".to_string();
            }
        }
    }

    /// Query the external references for the current position. Queries are
    /// delayed briefly so holding an arrow key does not fire a request for
    /// every intermediate position.
    fn maybe_request_cloud(&mut self) {
        if self.right_tab != RightTab::References {
            return;
        }
        let Some(fen) = self.current_fen() else {
            self.cloud = None;
            self.cloud_status = "No game is open.".to_string();
            return;
        };
        if self.cloud_rx.is_some() {
            return;
        }
        if self
            .cloud
            .as_ref()
            .is_some_and(|snapshot| snapshot.fen == fen)
        {
            return;
        }
        if let Some(snapshot) = self.cloud_cache.get(&fen).cloned() {
            self.cloud = Some(snapshot);
            self.cloud_status = "References loaded from cache.".to_string();
            self.cloud_pending = None;
            return;
        }
        let now = Instant::now();
        match self.cloud_pending.take() {
            Some((pending, started)) if pending == fen => {
                if now.duration_since(started) < Duration::from_millis(350) {
                    self.cloud_pending = Some((pending, started));
                    self.ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                self.start_cloud(fen);
            }
            _ => {
                self.cloud = None;
                self.cloud_status = "Waiting to query references…".to_string();
                self.cloud_pending = Some((fen, now));
                self.ctx.request_repaint_after(Duration::from_millis(350));
            }
        }
    }

    fn start_cloud(&mut self, fen: String) {
        if self.cloud_rx.is_some() {
            return;
        }
        self.cloud_job = Some(fen.clone());
        self.cloud_status = "Querying chessdb.cn and Lichess…".to_string();
        let token = self.lichess_token.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let snapshot = cloud::fetch(&fen, token.as_deref());
            let _ = tx.send(CloudMessage::Done(snapshot));
            ctx.request_repaint();
        });
        self.cloud_rx = Some(rx);
    }

    fn refresh_cloud(&mut self) {
        if let Some(fen) = self.current_fen() {
            self.cloud_cache.remove(&fen);
            self.cloud = None;
            self.cloud_pending = None;
            self.cloud_job = None;
            self.cloud_rx = None;
            self.start_cloud(fen);
        }
    }

    /// Load one game tree from the index, replacing the current game.
    fn select_game(&mut self, index: usize) {
        if index == self.game_idx && self.current.is_some() && self.prep.is_none() {
            return;
        }
        self.clear_review();
        self.recent_preview = None;
        self.annotation_editor = None;
        self.annotation_focus = false;
        if self.prep.take().is_some() {
            // A preparation board is disposable; loading a real game leaves it.
            self.dirty = false;
            self.current = None;
        }

        // Keep edits to the game we are leaving, so navigating the database does
        // not silently discard new moves or variations.
        if self.dirty {
            if let Some(game) = self.current.take() {
                self.edited.insert(self.game_idx, game);
            }
            self.dirty = false;
        }

        let loaded = match self.library.as_ref() {
            Some(library) if index < library.len() => Some(match self.edited.remove(&index) {
                Some(game) => Ok(game),
                None => library.load(index),
            }),
            _ => None,
        };
        let Some(loaded) = loaded else {
            return;
        };
        self.game_idx = index;
        self.right_tab = RightTab::Moves;
        self.goto(0);
        match loaded {
            Ok(game) => {
                self.status = if game.errors.is_empty() {
                    self.index_status.clone()
                } else {
                    format!(
                        "{} · game {}: {} import warnings",
                        self.index_status,
                        index + 1,
                        game.errors.len()
                    )
                };
                self.current = Some(game);
            }
            Err(err) => {
                self.current = None;
                self.status = format!("Could not load game {}: {err}", index + 1);
            }
        }
    }

    fn click_square(&mut self, sq: Square) {
        self.drag_from = None;
        if self.selected == Some(sq) {
            self.selected = None;
            self.targets.clear();
            return;
        }
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            return;
        };

        if let Some(from) = self.selected
            && from != sq
            && self.try_move(from, sq)
        {
            return;
        }

        if let Some(piece) = pos.board().piece_at(sq)
            && piece.color == pos.turn()
        {
            self.selected = Some(sq);
            self.targets = domain::legal_targets(&pos, sq)
                .into_iter()
                .map(|(to, _)| to)
                .collect();
            return;
        }

        self.selected = None;
        self.targets.clear();
    }

    /// Play a move if it is legal in the current position. Used by both click
    /// and drag-and-drop so the two input paths share exactly one move path.
    fn try_move(&mut self, from: Square, to: Square) -> bool {
        if self.recent_preview.is_some() {
            return false;
        }
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            return false;
        };
        let Some(mv) = domain::pick_move(&pos, from, to) else {
            return false;
        };
        let Some(game) = self.current.as_mut() else {
            return false;
        };
        let id = game.add_move(self.node, mv);
        self.dirty = true;
        self.goto(id);
        true
    }

    fn handle_board_event(&mut self, event: BoardEvent) {
        if self.recent_preview.is_some() {
            return;
        }
        match event {
            BoardEvent::Click(sq) => self.click_square(sq),
            BoardEvent::DragStart(from) => {
                self.drag_from = Some(from);
                self.selected = Some(from);
                let targets = self
                    .current
                    .as_ref()
                    .map(|game| {
                        domain::legal_targets(&game.nodes[self.node].pos, from)
                            .into_iter()
                            .map(|(to, _)| to)
                            .collect()
                    })
                    .unwrap_or_default();
                self.targets = targets;
            }
            BoardEvent::Drop { from, to } => {
                self.drag_from = None;
                self.selected = None;
                self.targets.clear();
                self.try_move(from, to);
            }
            BoardEvent::DragCancel => {
                self.drag_from = None;
                self.selected = None;
                self.targets.clear();
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let cancel = ctx.input(|i| i.key_pressed(Key::Escape));
        if cancel {
            if self.recent_preview.is_some() {
                self.close_recent_preview();
            }
            self.drag_from = None;
            self.selected = None;
            self.targets.clear();
            self.annotation_editor = None;
            self.annotation_focus = false;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (right, left, down, up, home, end, flip, tree, moves, annotate, engine, review) = ctx
            .input(|i| {
                (
                    i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                    i.key_pressed(Key::F),
                    i.key_pressed(Key::T),
                    i.key_pressed(Key::M),
                    i.key_pressed(Key::A),
                    i.key_pressed(Key::E),
                    i.key_pressed(Key::R),
                )
            });

        if right {
            self.next_move();
        }
        if left {
            self.prev_move();
        }
        if home {
            self.goto(0);
        }
        if end {
            self.go_end();
        }
        if up || down {
            let next = self.library.as_ref().filter(|l| !l.is_empty()).map(|l| {
                let n = l.len();
                if down {
                    (self.game_idx + 1) % n
                } else {
                    (self.game_idx + n - 1) % n
                }
            });
            if let Some(next) = next {
                self.select_game(next);
            }
        }
        if flip {
            self.orientation = self.orientation.other();
        }
        if tree {
            self.right_tab = RightTab::Tree;
        }
        if moves {
            self.right_tab = RightTab::Moves;
        }
        if annotate && self.current.is_some() && self.recent_preview.is_none() {
            self.right_tab = RightTab::Moves;
            self.annotation_editor = Some(self.node);
            self.annotation_focus = true;
        }
        if engine {
            self.right_tab = RightTab::Engine;
        }
        if review {
            self.right_tab = RightTab::Review;
        }
    }

    fn menu_bar(&mut self, root: &mut Ui) {
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open database…").clicked() {
                        if let Some(path) = open_dialog() {
                            self.load_file(&path);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Import from Lichess…").clicked() {
                        self.online_source = OnlineSource::Lichess;
                        self.online_open = true;
                        ui.close();
                    }
                    if ui.button("Import from Chess.com…").clicked() {
                        self.online_source = OnlineSource::ChessCom;
                        self.online_open = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Or drop a .pgn, .si4, .si3, or .cbh onto the window.");
                });

                ui.menu_button("Board", |ui| {
                    if ui.button("Flip board (F)").clicked() {
                        self.orientation = self.orientation.other();
                        ui.close();
                    }
                    if ui.button("Start position (Home)").clicked() {
                        self.goto(0);
                        ui.close();
                    }
                    ui.separator();
                    let has_clocks = self.current.as_ref().is_some_and(|game| game.has_clocks());
                    ui.add_enabled(
                        has_clocks,
                        egui::Checkbox::new(&mut self.show_clocks, "Show PGN clocks"),
                    )
                    .on_hover_text(if has_clocks {
                        "Show remaining time from [%clk] comments"
                    } else {
                        "This game has no [%clk] clock comments"
                    });
                    if !has_clocks {
                        self.show_clocks = false;
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(&self.status).weak());
                });
            });
        });
    }

    fn database_panel(&mut self, root: &mut Ui) {
        egui::Panel::left("games")
            .default_size(330.0)
            .size_range(260.0..=520.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.left_tab, LeftTab::Games, "Games")
                        .on_hover_text("Database games");
                    ui.selectable_value(&mut self.left_tab, LeftTab::Players, "Players")
                        .on_hover_text("Player index and preparation");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let count = match self.left_tab {
                            LeftTab::Games => self
                                .library
                                .as_ref()
                                .map(|l| l.len())
                                .unwrap_or(0)
                                .to_string(),
                            LeftTab::Players => self.players.len().to_string(),
                        };
                        ui.label(RichText::new(count).weak());
                    });
                });
                ui.separator();
                match self.left_tab {
                    LeftTab::Games => self.games_content(ui),
                    LeftTab::Players => self.players_content(ui),
                }
            });
    }

    fn games_content(&mut self, ui: &mut Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("filter players, event…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        let rows = self
            .library
            .as_ref()
            .map(|library| library.filter(&self.filter))
            .unwrap_or_default();

        if rows.is_empty() {
            let message = if self.filter.trim().is_empty() {
                "No games loaded."
            } else {
                "No matching games."
            };
            ui.label(RichText::new(message).weak());
            return;
        }

        let row_h = 42.0;
        let mut select = None;
        egui::ScrollArea::vertical()
            .id_salt("game-list")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for &idx in &rows[range] {
                    let row = self.library.as_ref().and_then(|l| l.get(idx)).map(|g| {
                        (
                            format!("{} – {}", g.white(), g.black()),
                            format!("{} · {}", g.event(), g.date()),
                        )
                    });
                    let Some((title, subtitle)) = row else {
                        continue;
                    };
                    let selected = idx == self.game_idx && self.prep.is_none();
                    let width = ui.available_width();
                    if game_row(ui, selected, &title, &subtitle, width, row_h - 2.0) {
                        select = Some(idx);
                    }
                }
            });
        if let Some(index) = select {
            self.select_game(index);
        }
    }

    fn players_content(&mut self, ui: &mut Ui) {
        let Some(index) = self.selected_player else {
            self.player_list(ui);
            return;
        };
        let Some(player) = self.players.get(index).cloned() else {
            self.selected_player = None;
            return;
        };

        let mut back = false;
        let mut start_white = false;
        let mut start_black = false;
        let mut exit = false;
        let mut select_game = None;
        let mut apply_reference = false;
        let mut save_reference = false;
        let mut clear_reference = false;

        ui.horizontal(|ui| {
            if ui.button("←").on_hover_text("All players").clicked() {
                back = true;
            }
            ui.heading(&player.name);
            if self
                .prep
                .as_ref()
                .is_some_and(|prep| prep.player_key == player.key)
                && ui.small_button("Exit prep").clicked()
            {
                exit = true;
            }
        });
        ui.label(
            RichText::new(format!(
                "{} · score {:.0}% · avg Elo {} · {}",
                games_label(player.games() as usize),
                player.score() * 100.0,
                player
                    .elo_avg()
                    .map(|elo| format!("{elo:.0}"))
                    .unwrap_or_else(|| "—".to_string()),
                player.years_label(),
            ))
            .weak(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "W {} · D {} · L {}",
                    player.wins(),
                    player.draws(),
                    player.losses()
                ))
                .strong(),
            );
            ui.label(
                RichText::new(format!(
                    "as White {}/{} · as Black {}/{}",
                    player.white.score() * 100.0,
                    player.white.games,
                    player.black.score() * 100.0,
                    player.black.games
                ))
                .weak()
                .small(),
            );
        });
        record_bar(ui, player.wins(), player.draws(), player.losses());
        let mut detail = Vec::new();
        if let Some(elo) = player.opponent_elo_avg() {
            detail.push(format!("opp. avg Elo {elo:.0}"));
        }
        if player.elo_max > 0 {
            detail.push(format!("peak Elo {}", player.elo_max));
        }
        if player.decided() > 0 {
            detail.push(format!("win ratio {:.0}%", player.win_ratio() * 100.0));
        }
        if !detail.is_empty() {
            ui.label(RichText::new(detail.join(" · ")).weak().small());
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Prepare as White")
                .on_hover_text(format!("You play White; {} is Black", player.name))
                .clicked()
            {
                start_white = true;
            }
            if ui
                .button("Prepare as Black")
                .on_hover_text(format!("You play Black; {} is White", player.name))
                .clicked()
            {
                start_black = true;
            }
        });
        ui.label(RichText::new(&self.player_book_status).weak().small());
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new("Reference database prior").strong())
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(
                    &mut self.reference_enabled,
                    "Use a separate reference database",
                )
                .on_hover_text(
                    "Reference games are indexed separately and filtered to the prepared player's rating window. Without one, the current database prior is used and labelled as potentially skewed.",
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.reference_path)
                        .hint_text("/path/to/masters.pgn, reference.si4, or database.cbh")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Rating window ±");
                    ui.add(
                        egui::DragValue::new(&mut self.reference_band)
                            .range(25..=1_000)
                            .speed(10.0)
                            .suffix(" Elo"),
                    );
                    ui.label("· offset");
                    ui.add(
                        egui::DragValue::new(&mut self.reference_offset)
                            .range(-1_000..=1_000)
                            .speed(5.0)
                            .suffix(" Elo"),
                    );
                });
                if let Some(elo) = player.elo_avg() {
                    let target = (elo.round() as i64 + i64::from(self.reference_offset))
                        .clamp(50, 4_000) as u32;
                    let filter = RatingFilter::around(target, self.reference_band);
                    ui.label(
                        RichText::new(format!(
                            "Player average {elo:.0} → reference window {}–{} Elo",
                            filter.min, filter.max
                        ))
                        .weak()
                        .small(),
                    );
                } else {
                    ui.label(
                        RichText::new("No player rating: a rating-filtered reference cannot be built.")
                            .weak()
                            .small(),
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Save & index for this player").clicked() {
                        apply_reference = true;
                        save_reference = true;
                    }
                    if ui.button("Clear reference").clicked() {
                        clear_reference = true;
                        save_reference = true;
                    }
                });
                ui.label(RichText::new(&self.reference_status).weak().small());
                if let Some(reference) = &self.reference {
                    ui.label(
                        RichText::new(format!(
                            "Loaded: {} · {} · prior window {}",
                            games_label(reference.index.game_count()),
                            positions_label(reference.index.position_count()),
                            reference.job.label()
                        ))
                        .weak()
                        .small(),
                    );
                }
            });
        if clear_reference {
            self.reference_path.clear();
            self.reference = None;
            self.reference_job = None;
            self.reference_rx = None;
            self.reference_status =
                "No reference database configured — using the current database prior (may be skewed)."
                    .to_string();
        }
        if save_reference {
            self.save_reference_prefs();
        }
        if apply_reference {
            self.ensure_reference_for_player(&player);
        }
        ui.separator();

        let row_h = 42.0;
        let total = player.game_indices.len();
        ui.label(RichText::new(format!("Games ({total})")).strong());
        if total == 0 {
            ui.label(RichText::new("No games for this player.").weak());
        } else {
            egui::ScrollArea::vertical()
                .id_salt("player-games")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, total, |ui, range| {
                    for &idx in &player.game_indices[range] {
                        let Some(entry) = self.library.as_ref().and_then(|l| l.get(idx)) else {
                            continue;
                        };
                        let player_white = players::player_key(entry.white()).as_deref()
                            == Some(player.key.as_str());
                        let (opponent, color) = if player_white {
                            (entry.black(), "White")
                        } else {
                            (entry.white(), "Black")
                        };
                        let title = format!("vs {opponent} · as {color}");
                        let subtitle =
                            format!("{} · {} · {}", entry.result(), entry.event(), entry.date());
                        let selected = idx == self.game_idx && self.prep.is_none();
                        let width = ui.available_width();
                        if game_row(ui, selected, &title, &subtitle, width, row_h - 2.0) {
                            select_game = Some(idx);
                        }
                    }
                });
        }

        if back {
            if prep_matches(self, &player.key) {
                exit = true;
            }
            self.selected_player = None;
            self.player_filter.clear();
        }
        if exit {
            self.exit_prep();
        }
        if start_white {
            self.start_prep(true);
        }
        if start_black {
            self.start_prep(false);
        }
        if let Some(index) = select_game {
            self.select_game(index);
        }
    }

    fn player_list(&mut self, ui: &mut Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.player_filter)
                .hint_text("search players…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        if self.players.is_empty() {
            ui.label(RichText::new("No players indexed.").weak());
            return;
        }
        let rows = self.players.filter(&self.player_filter);
        if rows.is_empty() {
            ui.label(RichText::new("No matching players.").weak());
            return;
        }

        let row_h = 46.0;
        let mut select = None;
        egui::ScrollArea::vertical()
            .id_salt("player-list")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for &index in &rows[range] {
                    let Some(player) = self.players.get(index) else {
                        continue;
                    };
                    let title = player.name.clone();
                    let subtitle = format!(
                        "{} · score {:.0}% · Elo {} · {}",
                        games_label(player.games() as usize),
                        player.score() * 100.0,
                        player
                            .elo_avg()
                            .map(|elo| format!("{elo:.0}"))
                            .unwrap_or_else(|| "—".to_string()),
                        player.years_label()
                    );
                    let selected = self.selected_player == Some(index);
                    let width = ui.available_width();
                    if game_row(ui, selected, &title, &subtitle, width, row_h - 2.0) {
                        select = Some(index);
                    }
                }
            });
        if let Some(index) = select {
            self.select_player(index);
        }
    }

    fn moves_panel(&mut self, root: &mut Ui) {
        egui::Panel::right("moves")
            .default_size(390.0)
            .size_range(280.0..=560.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.right_tab, RightTab::Moves, "Moves")
                        .on_hover_text("Move list (M)");
                    ui.selectable_value(&mut self.right_tab, RightTab::Tree, "Tree")
                        .on_hover_text("Tree (T)");
                    ui.selectable_value(&mut self.right_tab, RightTab::Engine, "Engine")
                        .on_hover_text("UCI engine (E)");
                    ui.selectable_value(&mut self.right_tab, RightTab::Review, "Review")
                        .on_hover_text("Game review (R)");
                    ui.selectable_value(&mut self.right_tab, RightTab::References, "Refs")
                        .on_hover_text("chessdb.cn and Lichess references for this position");
                });
                ui.separator();
                match self.right_tab {
                    RightTab::Moves if self.recent_preview.is_some() => self.preview_content(ui),
                    RightTab::Moves => self.moves_content(ui),
                    RightTab::Tree => self.tree_content(ui),
                    RightTab::Engine => self.engine_content(ui),
                    RightTab::Review => self.review_content(ui),
                    RightTab::References => self.references_content(ui),
                }
            });
    }

    fn references_content(&mut self, ui: &mut Ui) {
        self.maybe_request_cloud();

        let snapshot = self.cloud.clone();
        let position = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone());
        let status = self.cloud_status.clone();
        let loading = self.cloud_rx.is_some();
        let token_configured = self.lichess_token.is_some();
        let config_path = crate::settings::cloud_path();
        let mut clicked_uci = None;
        let mut refresh = false;

        egui::ScrollArea::vertical()
            .id_salt("cloud-references")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("References");
                    if loading {
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Refresh").clicked() {
                            refresh = true;
                        }
                    });
                });
                ui.label(RichText::new(&status).small().weak());
                if let Some(position) = &position {
                    ui.label(RichText::new(fen_string(position)).monospace().small().weak())
                        .on_hover_text("Current FEN");
                }
                ui.add_space(6.0);

                let Some(snapshot) = &snapshot else {
                    ui.label(
                        RichText::new(
                            "Move the board or open a game; references are queried automatically.",
                        )
                        .weak(),
                    );
                    return;
                };

                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(10.0)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("chessdb.cn");
                            ui.label(RichText::new("cloud database").small().weak());
                        });
                        match &snapshot.chessdb {
                            Ok(data) => {
                                ui.label(
                                    RichText::new(format!(
                                        "{} known move{} · score from the side to move",
                                        data.moves.len(),
                                        if data.moves.len() == 1 { "" } else { "s" }
                                    ))
                                    .small()
                                    .weak(),
                                );
                                ui.add_space(3.0);
                                egui::Grid::new("references-chessdb-grid")
                                    .num_columns(5)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for heading in ["Move", "Score", "Winrate", "Rank", "Note"] {
                                            ui.label(RichText::new(heading).strong().small());
                                        }
                                        ui.end_row();
                                        for mv in data.moves.iter().take(30) {
                                            let label = position
                                                .as_ref()
                                                .and_then(|pos| cloud::uci_san(pos, &mv.uci))
                                                .unwrap_or_else(|| mv.uci.clone());
                                            if ui.selectable_label(false, label).clicked() {
                                                clicked_uci = Some(mv.uci.clone());
                                            }
                                            ui.label(format_cp(mv.score));
                                            ui.label(format!("{:.1}%", mv.winrate));
                                            ui.label(mv.rank.to_string());
                                            ui.label(RichText::new(&mv.note).small().weak());
                                            ui.end_row();
                                        }
                                    });
                            }
                            Err(err) => {
                                ui.label(RichText::new(err).small().weak());
                            }
                        }
                    });

                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(10.0)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Lichess");
                            ui.label(RichText::new("opening explorer").small().weak());
                        });
                        match &snapshot.explorer {
                            Ok(data) => {
                                if let Some(opening) = &data.opening
                                    && !opening.name.is_empty()
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {}",
                                            opening.eco, opening.name
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{} games · White {} · Draw {} · Black {}",
                                        data.total(),
                                        format_percent(data.white, data.total()),
                                        format_percent(data.draws, data.total()),
                                        format_percent(data.black, data.total())
                                    ))
                                    .small()
                                    .weak(),
                                );
                                ui.add_space(3.0);
                                if data.moves.is_empty() {
                                    ui.label(RichText::new("No Lichess games at this position.").weak());
                                } else {
                                    egui::Grid::new("references-lichess-grid")
                                        .num_columns(5)
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for heading in
                                                ["Move", "Games", "W / D / B", "Score", "Avg"]
                                            {
                                                ui.label(
                                                    RichText::new(heading).strong().small(),
                                                );
                                            }
                                            ui.end_row();
                                            for mv in data.moves.iter().take(20) {
                                                if ui
                                                    .selectable_label(false, &mv.san)
                                                    .clicked()
                                                {
                                                    clicked_uci = Some(mv.uci.clone());
                                                }
                                                ui.label(mv.total().to_string());
                                                ui.label(format!(
                                                    "{} / {} / {}",
                                                    format_percent(mv.white, mv.total()),
                                                    format_percent(mv.draws, mv.total()),
                                                    format_percent(mv.black, mv.total())
                                                ));
                                                ui.label(format!(
                                                    "{:.1}%",
                                                    mv.score() * 100.0
                                                ));
                                                ui.label(
                                                    mv.average_rating
                                                        .map(|rating| rating.to_string())
                                                        .unwrap_or_else(|| "—".to_string()),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }
                                if !data.top_games.is_empty() {
                                    ui.add_space(4.0);
                                    ui.label(RichText::new("Top games").strong().small());
                                    for game in &data.top_games {
                                        let date = match (game.year, game.month) {
                                            (Some(year), Some(month)) => {
                                                format!(" · {year}-{month:02}")
                                            }
                                            (Some(year), None) => format!(" · {year}"),
                                            _ => String::new(),
                                        };
                                        let white = match game.white_rating {
                                            Some(rating) => format!("{} ({rating})", game.white),
                                            None => game.white.clone(),
                                        };
                                        let black = match game.black_rating {
                                            Some(rating) => format!("{} ({rating})", game.black),
                                            None => game.black.clone(),
                                        };
                                        ui.hyperlink_to(
                                            format!("{white} – {black}{date}"),
                                            format!("https://lichess.org/{}", game.id),
                                        );
                                    }
                                }
                            }
                            Err(err) => {
                                if token_configured {
                                    ui.label(RichText::new(err).small().weak());
                                } else {
                                    ui.label(
                                        RichText::new(
                                            "Lichess opening-explorer requests need an API token. The public cloud evaluation below works without one.",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                    if let Some(path) = &config_path {
                                        ui.label(
                                            RichText::new(format!(
                                                "Add \"lichess_token\" to {} or set BRETT_LICHESS_TOKEN.",
                                                path.display()
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    }
                                }
                            }
                        }
                    });

                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(10.0)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Lichess cloud eval");
                            ui.label(RichText::new("engine evaluation").small().weak());
                        });
                        match &snapshot.cloud_eval {
                            Ok(eval) => {
                                ui.label(
                                    RichText::new(format!(
                                        "depth {} · {} knodes",
                                        eval.depth,
                                        eval.knodes
                                    ))
                                    .small()
                                    .weak(),
                                );
                                for (index, pv) in eval.pvs.iter().enumerate() {
                                    let first = pv.moves.split_whitespace().next();
                                    let san = first
                                        .and_then(|uci| {
                                            position
                                                .as_ref()
                                                .and_then(|pos| cloud::uci_san(pos, uci))
                                        })
                                        .unwrap_or_else(|| {
                                            first.unwrap_or("—").to_string()
                                        });
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{}.", index + 1)).weak(),
                                        );
                                        if let Some(uci) = first
                                            && ui.selectable_label(false, san).clicked()
                                        {
                                            clicked_uci = Some(uci.to_string());
                                        }
                                        ui.label(pv_score(pv.cp, pv.mate));
                                        ui.label(
                                            RichText::new(&pv.moves).small().weak(),
                                        );
                                    });
                                }
                            }
                            Err(err) => {
                                ui.label(RichText::new(err).small().weak());
                            }
                        }
                    });

                ui.add_space(8.0);
                if let Some(path) = &config_path {
                    ui.label(
                        RichText::new(format!(
                            "Lichess token: {} (or BRETT_LICHESS_TOKEN)",
                            path.display()
                        ))
                        .small()
                        .weak(),
                    );
                }
            });

        if refresh {
            self.refresh_cloud();
        }
        if let Some(uci) = clicked_uci {
            self.play_uci(&uci);
        }
    }

    fn engine_content(&mut self, ui: &mut Ui) {
        self.maybe_engine_analysis();

        let mut start = false;
        let mut stop = false;
        let mut settings_changed = false;
        let mut clicked_uci = None;
        let mut browse_engine = false;
        let mut apply_engine = false;

        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(10.0)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Engine");
                    if self.engine_searching {
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(if self.engine_ready {
                                "ready"
                            } else if self.engine.is_some() {
                                "starting"
                            } else {
                                "stopped"
                            })
                            .small()
                            .weak(),
                        );
                    });
                });
                ui.label(
                    RichText::new(if self.engine_name.is_empty() {
                        "UCI engine"
                    } else {
                        &self.engine_name
                    })
                    .small()
                    .weak(),
                )
                .on_hover_text("UCI engine selected in Engine settings");
                ui.label(RichText::new(&self.engine_status).small().weak());
                ui.add_space(4.0);

                egui::CollapsingHeader::new(RichText::new("Engine and hardware").strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Executable");
                            let width = (ui.available_width() - 8.0).max(120.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.engine_path)
                                    .desired_width(width)
                                    .hint_text("/usr/bin/stockfish"),
                            )
                            .on_hover_text("Path to any UCI-compatible engine");
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Browse…").clicked() {
                                browse_engine = true;
                            }
                            if ui
                                .button("Default Stockfish")
                                .on_hover_text(
                                    "Reset the path to $BRETT_ENGINE or /usr/bin/stockfish",
                                )
                                .clicked()
                            {
                                self.engine_path = crate::engine::default_engine_path()
                                    .display()
                                    .to_string();
                            }
                            if ui
                                .button("Apply & restart")
                                .on_hover_text(
                                    "Save the executable, threads, and hash size, then restart the engine",
                                )
                                .clicked()
                            {
                                apply_engine = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Threads");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_threads)
                                    .range(1..=256)
                                    .speed(1),
                            )
                            .on_hover_text(
                                "UCI Threads option. Applied when the engine advertises it.",
                            );
                            ui.label("Hash / RAM");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_hash_mb)
                                    .range(1..=32_768)
                                    .suffix(" MB")
                                    .speed(64),
                            )
                            .on_hover_text(
                                "UCI Hash option: transposition-table memory in megabytes.",
                            );
                        });
                        if let Some(engine) = &self.engine {
                            let active = engine.options();
                            ui.label(
                                RichText::new(format!(
                                    "Active: {} · {} threads · {} MB hash",
                                    engine.path().display(),
                                    active.threads,
                                    active.hash_mb
                                ))
                                .small()
                                .weak(),
                            );
                        }
                        if self.engine_settings_pending() {
                            ui.label(
                                RichText::new("Settings changed — Apply & restart to use them.")
                                    .small()
                                    .color(GOLD),
                            );
                        } else if self.engine.is_none() {
                            ui.label(
                                RichText::new(
                                    "The engine starts when analysis is requested or Apply is pressed.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                    });
                ui.add_space(4.0);

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .checkbox(&mut self.engine_auto, "Auto analyze")
                        .on_hover_text("Re-analyze when the position changes")
                        .changed()
                    {
                        settings_changed = true;
                    }
                    ui.label("MultiPV");
                    if ui
                        .add(egui::DragValue::new(&mut self.engine_multipv).range(1..=5))
                        .changed()
                    {
                        settings_changed = true;
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("engine-limit")
                        .selected_text(if self.engine_use_time {
                            "Time"
                        } else {
                            "Depth"
                        })
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(&mut self.engine_use_time, false, "Depth")
                                .changed()
                            {
                                settings_changed = true;
                            }
                            if ui
                                .selectable_value(&mut self.engine_use_time, true, "Time")
                                .changed()
                            {
                                settings_changed = true;
                            }
                        });
                    if self.engine_use_time {
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.engine_time_ms)
                                    .range(100..=30_000)
                                    .suffix(" ms")
                                    .speed(50),
                            )
                            .changed()
                        {
                            settings_changed = true;
                        }
                    } else if ui
                        .add(
                            egui::DragValue::new(&mut self.engine_depth)
                                .range(6..=40)
                                .speed(1),
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                });

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.engine_searching, egui::Button::new("Analyze"))
                        .clicked()
                    {
                        start = true;
                    }
                    if ui
                        .add_enabled(self.engine_searching, egui::Button::new("Stop"))
                        .clicked()
                    {
                        stop = true;
                    }
                    if self.engine.is_none() && ui.button("Start engine").clicked() {
                        start = true;
                    }
                });
            });

        if browse_engine && let Some(path) = open_engine_dialog() {
            self.engine_path = path.display().to_string();
        }
        if apply_engine {
            self.apply_engine_settings();
        }
        if settings_changed {
            self.clear_engine_results();
        }
        if start {
            self.request_engine_analysis();
        }
        if stop {
            if let Some(engine) = &self.engine {
                engine.stop();
            }
            self.engine_searching = false;
            self.engine_status = "Stopped.".to_string();
        }

        ui.add_space(8.0);
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            ui.label(RichText::new("Open a game to analyze a position.").weak());
            return;
        };

        let infos: Vec<EngineInfo> = self.engine_infos.values().cloned().collect();
        if infos.is_empty() {
            ui.label(
                RichText::new(if self.engine_searching {
                    "Waiting for engine output…"
                } else {
                    "No analysis yet. Press Analyze."
                })
                .weak()
                .italics(),
            );
        } else {
            for info in infos {
                let score = engine_score_label(info.score, pos.turn());
                let color = engine_eval_color(info.score, pos.turn());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(score).monospace().strong().color(color));
                    ui.label(RichText::new(format!("d{}", info.depth)).small().weak());
                    let line = pv_san(&pos, &info.pv, 12);
                    if ui
                        .selectable_label(
                            false,
                            RichText::new(if line.is_empty() { "…" } else { &line }).monospace(),
                        )
                        .on_hover_text("Play the first move of this line")
                        .clicked()
                        && let Some(first) = info.pv.first()
                    {
                        clicked_uci = Some(first.clone());
                    }
                });
                let mut stats = Vec::new();
                if let Some(nodes) = info.nodes {
                    stats.push(format!("{nodes} nodes"));
                }
                if let Some(time) = info.time_ms
                    && time > 0
                {
                    stats.push(format!("{time} ms"));
                }
                if let Some(nps) = info.nps {
                    stats.push(format!("{nps} nps"));
                }
                if !stats.is_empty() {
                    ui.label(RichText::new(stats.join(" · ")).small().weak());
                }
                ui.add_space(3.0);
            }
        }

        if let Some(best) = &self.engine_bestmove {
            ui.label(
                RichText::new(format!("bestmove {best}"))
                    .monospace()
                    .small()
                    .weak(),
            );
        }
        if let Some(uci) = clicked_uci {
            self.play_uci(&uci);
        }
    }

    fn review_content(&mut self, ui: &mut Ui) {
        let running = self.review.as_ref().is_some_and(|review| review.running);
        let can_write_nags = self.recent_preview.is_none();
        let mut start = false;
        let mut cancel = false;
        let mut write_nags = false;

        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(10.0)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Game review");
                    if running {
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(if running { "analyzing" } else { "idle" })
                                .small()
                                .weak(),
                        );
                    });
                });
                ui.label(
                    RichText::new("Stockfish grades every mainline move by centipawn loss.")
                        .small()
                        .weak(),
                );
                ui.add_space(4.0);

                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("review-limit")
                            .selected_text(if self.review_use_time {
                                "Time"
                            } else {
                                "Depth"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.review_use_time, false, "Depth");
                                ui.selectable_value(&mut self.review_use_time, true, "Time");
                            });
                        if self.review_use_time {
                            ui.add(
                                egui::DragValue::new(&mut self.review_time_ms)
                                    .range(100..=10_000)
                                    .suffix(" ms")
                                    .speed(50),
                            );
                        } else {
                            ui.add(
                                egui::DragValue::new(&mut self.review_depth)
                                    .range(6..=30)
                                    .speed(1),
                            );
                        }
                    });
                });

                ui.horizontal(|ui| {
                    let label = if self.review.is_some() {
                        "Restart review"
                    } else {
                        "Analyze game"
                    };
                    if ui
                        .add_enabled(self.current.is_some(), egui::Button::new(label))
                        .clicked()
                    {
                        start = true;
                    }
                    if ui
                        .add_enabled(running, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
                ui.label(RichText::new(&self.review_status).small().weak());
                ui.label(
                    RichText::new(format!(
                        "{} · {} threads · {} MB hash",
                        self.engine_path().display(),
                        self.engine_threads,
                        self.engine_hash_mb
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text("Change the engine in the Engine tab");
            });

        if start {
            self.start_review();
        }
        if cancel {
            self.cancel_review();
        }

        let mut clicked_node = None;
        if let Some(review) = &self.review
            && let Some(game) = &self.current
        {
            let moves = review.moves(game);
            let (done, total) = review.progress();
            let fraction = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            ui.add_space(8.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(format!("{done} / {total} positions analyzed"))
                    .desired_width(ui.available_width()),
            );

            ui.horizontal_wrapped(|ui| {
                match review::accuracy(&moves) {
                    Some(accuracy) => {
                        ui.label(
                            RichText::new(format!("Accuracy {accuracy:.1}%"))
                                .strong()
                                .color(GOLD),
                        )
                        .on_hover_text(
                            "Heuristic estimate from centipawn loss, not an official rating",
                        );
                    }
                    None => {
                        ui.label(RichText::new("Accuracy pending").weak());
                    }
                }
                let counts = review.quality_counts(&moves);
                for quality in Quality::ALL {
                    let count = counts[quality.index()];
                    if count > 0 {
                        ui.label(
                            RichText::new(format!("{} {count}", quality.symbol()))
                                .small()
                                .color(quality_color(quality)),
                        )
                        .on_hover_text(quality.label());
                    }
                }
            });

            ui.add_space(6.0);
            review_graph(ui, review);

            if !running
                && can_write_nags
                && !moves.is_empty()
                && ui
                    .button("Write NAGs to mainline")
                    .on_hover_text("Add !! / ! / ?! / ? / ?? annotations to the reviewed moves")
                    .clicked()
            {
                write_nags = true;
            }

            if moves.is_empty() {
                ui.label(
                    RichText::new("Move grades appear as each position is analyzed.")
                        .small()
                        .weak()
                        .italics(),
                );
            } else {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("review-moves")
                    .max_height(250.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for mov in &moves {
                            let number = if mov.color == Color::White {
                                format!("{}. ", mov.number)
                            } else {
                                format!("{}… ", mov.number)
                            };
                            let eval = mov
                                .eval_before
                                .map(|score| engine_score_label(Some(score), mov.color))
                                .unwrap_or_else(|| "—".to_string());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(mov.quality.symbol())
                                        .monospace()
                                        .strong()
                                        .color(quality_color(mov.quality)),
                                )
                                .on_hover_text(mov.quality.label());
                                let label = format!("{number}{}  {eval}", mov.san);
                                let best = mov.best.as_deref().unwrap_or("?");
                                if ui
                                    .selectable_label(
                                        self.node == mov.node,
                                        RichText::new(label).monospace().small(),
                                    )
                                    .on_hover_text(format!(
                                        "{} · played {} · best {best}",
                                        mov.quality.label(),
                                        mov.played
                                    ))
                                    .clicked()
                                {
                                    clicked_node = Some(mov.node);
                                }
                                if let Some(loss) = mov.cp_loss {
                                    ui.label(
                                        RichText::new(format!("-{loss}"))
                                            .monospace()
                                            .small()
                                            .color(quality_color(mov.quality)),
                                    );
                                }
                            });
                        }
                    });
            }
        }

        if write_nags {
            self.write_review_nags();
        }
        if let Some(node) = clicked_node {
            self.goto(node);
        }
    }

    fn write_review_nags(&mut self) {
        let moves = self
            .review
            .as_ref()
            .and_then(|review| self.current.as_ref().map(|game| review.moves(game)))
            .unwrap_or_default();
        let actions: Vec<(usize, u8)> = moves
            .iter()
            .filter_map(|mov| quality_nag(mov.quality).map(|nag| (mov.node, nag)))
            .collect();
        let mut changed = 0usize;
        for (node, nag) in actions {
            let already = self
                .current
                .as_ref()
                .and_then(|game| game.nodes.get(node))
                .is_some_and(|node| node.nags.contains(&nag));
            if already {
                continue;
            }
            self.set_nag(node, Some(nag));
            changed += 1;
        }
        self.review_status = if changed == 0 {
            "Review NAGs already match the current grades.".to_string()
        } else {
            format!("Wrote {changed} review annotations to the mainline.")
        };
    }

    fn moves_content(&mut self, ui: &mut Ui) {
        let current_node = self.node;
        let prep = self.prep.clone();
        let Some(game) = self.current.clone() else {
            ui.heading("No game");
            return;
        };
        let node = current_node.min(game.nodes.len().saturating_sub(1));
        let editor = self.annotation_editor;
        let focus_comment = self.annotation_focus && editor == Some(node);
        self.annotation_focus = false;

        let mut clicked = None;
        let mut open_editor = None;
        let mut close_editor = false;
        let mut nag_action: Option<(usize, Option<u8>)> = None;
        let mut comment_update: Option<(usize, String)> = None;
        let mut tag_update: Option<(usize, String)> = None;
        let mut tag_delete: Option<usize> = None;
        let mut tag_add: Option<&'static str> = None;

        game_header_card(ui, &game, prep.as_ref());
        ui.add_space(6.0);
        game_tags_card(ui, &game, &mut tag_update, &mut tag_delete, &mut tag_add);
        ui.add_space(8.0);

        let lines = domain::move_lines(&game);
        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(10.0)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Moves").strong());
                    ui.label(
                        RichText::new(format!("{} plies", game.mainline_len()))
                            .weak()
                            .small(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("Annotate")
                            .on_hover_text("Edit the selected move")
                            .clicked()
                        {
                            open_editor = Some(node);
                        }
                    });
                });
                ui.add_space(2.0);

                let root_comment = game.nodes[0].comment.trim();
                if !root_comment.is_empty() {
                    let preview = comment_preview(root_comment, 72);
                    let response = ui.add(
                        egui::Label::new(
                            RichText::new(format!("“{preview}”"))
                                .italics()
                                .small()
                                .color(COMMENT_TEXT),
                        )
                        .sense(Sense::click()),
                    );
                    if response.clicked() {
                        open_editor = Some(0);
                    }
                    response.on_hover_text(root_comment);
                }

                egui::ScrollArea::vertical()
                    .id_salt("moves")
                    .max_height((ui.available_height() * 0.42).clamp(100.0, 330.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if lines.is_empty() {
                            ui.label(
                                RichText::new("No moves yet. Play a move on the board.")
                                    .weak()
                                    .italics(),
                            );
                            return;
                        }
                        ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                        for line in &lines {
                            render_move_line(
                                ui,
                                line,
                                &game,
                                node,
                                &mut clicked,
                                &mut open_editor,
                                &mut nag_action,
                            );
                        }
                    });
            });

        ui.add_space(8.0);

        let edit_node = editor
            .unwrap_or(node)
            .min(game.nodes.len().saturating_sub(1));
        let editing = editor.is_some();
        let selected_san = game.nodes[edit_node]
            .mv
            .as_ref()
            .map(|ply| domain::figurine_san(&ply.san))
            .unwrap_or_else(|| "Start position".to_string());
        let selected_nags = game.nodes[edit_node].nags.clone();
        let symbols = nag_symbols(&selected_nags);
        let mut comment = game.nodes[edit_node].comment.clone();

        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(10.0)
            .stroke(Stroke::new(
                if editing { 1.5 } else { 1.0 },
                if editing { GOLD } else { BORDER },
            ))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Annotations").strong());
                    ui.label(RichText::new(selected_san).monospace().color(GOLD));
                    if !symbols.is_empty() {
                        ui.label(RichText::new(symbols).strong().color(GOLD));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if editing && ui.small_button("Done").clicked() {
                            close_editor = true;
                        }
                        if ui
                            .small_button("Clear")
                            .on_hover_text("Remove all annotations from this move")
                            .clicked()
                        {
                            nag_action = Some((edit_node, None));
                        }
                    });
                });
                ui.label(
                    RichText::new(if editing {
                        "Annotation editing · choose a symbol below"
                    } else {
                        "Right-click a move for the symbol palette"
                    })
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);

                if edit_node > 0 {
                    egui::ScrollArea::vertical()
                        .id_salt("nag-palette")
                        .max_height(250.0)
                        .show(ui, |ui| {
                            if let Some(nag) = nag_palette(ui, &selected_nags, false) {
                                nag_action = Some((edit_node, Some(nag)));
                            }
                        });
                }

                ui.add_space(5.0);
                ui.label(RichText::new("Comment").small().strong().weak());
                let response = ui.add(
                    egui::TextEdit::multiline(&mut comment)
                        .id_salt(("annotation-comment", edit_node))
                        .hint_text("Add a note for this move…")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    comment_update = Some((edit_node, comment.clone()));
                }
                if focus_comment && edit_node == node {
                    response.request_focus();
                }
            });

        ui.add_space(6.0);
        let fen = fen_string(&game.nodes[node].pos);
        ui.collapsing("FEN", |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(0x18, 0x1A, 0x1C))
                .corner_radius(6.0)
                .inner_margin(7.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(fen.clone());
                        }
                        ui.label(
                            RichText::new(&fen)
                                .monospace()
                                .small()
                                .color(Color32::from_gray(195)),
                        );
                    });
                });
        });

        if tag_update.is_some() || tag_delete.is_some() || tag_add.is_some() {
            if let Some(game) = self.current.as_mut() {
                if let Some((index, value)) = tag_update
                    && let Some((_, current)) = game.headers.get_mut(index)
                {
                    *current = value;
                }
                if let Some(index) = tag_delete
                    && index < game.headers.len()
                {
                    game.headers.remove(index);
                }
                if let Some(key) = tag_add {
                    game.set_header(key, String::new());
                }
            }
            self.dirty = true;
        }

        if let Some(action) = nag_action {
            self.set_nag(action.0, action.1);
        }
        if close_editor {
            self.annotation_editor = None;
            self.annotation_focus = false;
        }
        if let Some((node, text)) = comment_update
            && let Some(game) = self.current.as_mut()
            && let Some(node) = game.nodes.get_mut(node)
        {
            node.replace_comment(&text);
            self.dirty = true;
        }
        if let Some(node) = open_editor {
            self.goto(node);
            self.annotation_editor = Some(node);
            self.annotation_focus = true;
        } else if let Some(node) = clicked {
            self.goto(node);
        }
    }

    fn set_nag(&mut self, node: usize, nag: Option<u8>) {
        let Some(game) = self.current.as_mut() else {
            return;
        };
        let Some(node) = game.nodes.get_mut(node) else {
            return;
        };
        match nag {
            None => node.nags.clear(),
            Some(nag) => {
                if let Some(position) = node.nags.iter().position(|&existing| existing == nag) {
                    node.nags.remove(position);
                } else {
                    node.nags.retain(|&existing| !nag_conflicts(existing, nag));
                    node.nags.push(nag);
                    node.nags.sort_unstable();
                }
            }
        }
        self.dirty = true;
    }

    fn tree_content(&mut self, ui: &mut Ui) {
        let Some(game) = self.game() else {
            ui.heading("No game");
            return;
        };
        let pos = game.nodes[self.node].pos.clone();
        let key = position::position_key(&pos);
        if self.recent_preview.is_none()
            && let Some(prep) = self.prep.clone()
        {
            self.prep_tree(ui, &prep, &pos, key);
            return;
        }

        ui.heading("Tree");
        ui.label(RichText::new(&self.index_status).weak());

        if self.index.is_none() {
            ui.add_space(8.0);
            ui.label(if self.index_rx.is_some() {
                "Building the position index in the background. You can keep browsing."
            } else {
                "No position index is loaded."
            });
            if let Some(job) = &self.index_job {
                ui.label(RichText::new(job.display().to_string()).weak().small());
            }
            return;
        }

        let mut clicked = None;
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("opening-tree")
            .max_height((ui.available_height() - 8.0).max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                clicked = opening_tree_table(
                    ui,
                    self.index.as_ref(),
                    key,
                    &pos,
                    "No indexed game reached this position.",
                );
            });

        if let Some(mv) = clicked {
            self.play_move(mv);
        }
    }

    /// Build the prepared player's moves from the root to the current node.
    /// Likelihoods are evaluated at the parent position, which is the same
    /// distribution that was shown before the move was played.
    fn prep_line_rows(&self, prep: &PrepSession) -> Vec<PrepLineRow> {
        let Some(game) = self.current.as_ref() else {
            return Vec::new();
        };
        let book = self
            .player_book
            .as_ref()
            .filter(|book| book.player_key() == prep.player_key);
        let index = self.index.as_ref();

        let mut line_nodes = Vec::new();
        let mut node = self.node.min(game.nodes.len().saturating_sub(1));
        while let Some(parent) = game.nodes[node].parent {
            line_nodes.push(node);
            node = parent;
        }
        line_nodes.reverse();

        let mut rows = Vec::new();
        for node_id in line_nodes {
            let current = &game.nodes[node_id];
            let Some(ply) = &current.mv else {
                continue;
            };
            if ply.color != prep.opponent_color() {
                continue;
            }
            let Some(parent) = current.parent else {
                continue;
            };
            let parent_pos = &game.nodes[parent].pos;
            let parent_key = position::position_key(parent_pos);
            let Ok(uci) = ply.uci.parse::<UciMove>() else {
                continue;
            };
            let Ok(mv) = uci.to_move(parent_pos) else {
                continue;
            };
            let Some(code) = position::encode_move(mv) else {
                continue;
            };

            let tree = index
                .map(|index| index.moves(parent_key))
                .unwrap_or_default()
                .into_iter()
                .find(|stat| stat.mv == code);
            let player_move = book.and_then(|book| {
                book.moves(parent_key)
                    .into_iter()
                    .find(|stat| stat.mv == code)
            });
            let predicted = book.and_then(|book| {
                let (prior, prior_source) = self.prior_for_position(parent_key);
                players::predict(
                    book,
                    parent_key,
                    &prior,
                    prior_source,
                    parent_pos.legal_moves().len(),
                )
                .moves
                .into_iter()
                .find(|predicted| predicted.mv == code)
            });
            let likelihood = predicted.map(|predicted| predicted.probability);
            let likelihood_low = predicted.map(|predicted| predicted.probability_low);
            let likelihood_high = predicted.map(|predicted| predicted.probability_high);
            let date = player_move
                .map(|stat| stat.last_date)
                .filter(|&date| date != 0)
                .or_else(|| tree.map(|stat| stat.last_date).filter(|&date| date != 0))
                .unwrap_or(0);

            rows.push(PrepLineRow {
                label: format!(
                    "{}{} {}",
                    ply.number,
                    if ply.color == Color::White {
                        "."
                    } else {
                        "..."
                    },
                    domain::figurine_san(&ply.san)
                ),
                date,
                tree_score: tree.map(|stat| stat.score_white()),
                likelihood,
                likelihood_low,
                likelihood_high,
            });
        }
        rows
    }

    fn prep_prediction_data(
        &self,
        prep: &PrepSession,
        pos: &Chess,
        key: u64,
    ) -> Option<PrepPredictionData> {
        let book = self
            .player_book
            .as_ref()
            .filter(|book| book.player_key() == prep.player_key)?;
        let (prior, prior_source) = self.prior_for_position(key);
        let warning = self.prior_warning(prior_source);
        let prediction = players::predict(book, key, &prior, prior_source, pos.legal_moves().len());
        let stats = book.stats(key);
        let tree_moves = self
            .index
            .as_ref()
            .map(|index| index.moves(key))
            .unwrap_or_default();
        let rows = prediction
            .moves
            .iter()
            .take(24)
            .filter_map(|predicted| {
                let mv = position::decode_move(pos, predicted.mv)?;
                let san = SanPlus::from_move(pos.clone(), mv).to_string();
                let tree = tree_moves
                    .iter()
                    .find(|stat| stat.mv == predicted.mv)
                    .copied();
                Some(PrepPredictionRow {
                    mv,
                    san,
                    probability: predicted.probability,
                    probability_low: predicted.probability_low,
                    probability_high: predicted.probability_high,
                    player_games: predicted.player_games,
                    recent_games: predicted.recent_games,
                    prior_games: predicted.prior_games,
                    last_date: predicted.last_date,
                    trend_pp: predicted.trend_pp,
                    tree,
                })
            })
            .collect();
        Some(PrepPredictionData {
            rows,
            stats,
            prediction,
            warning,
        })
    }

    fn prep_recent_rows(&self, prep: &PrepSession, key: u64, limit: usize) -> Vec<PrepRecentRow> {
        let Some(book) = self
            .player_book
            .as_ref()
            .filter(|book| book.player_key() == prep.player_key)
        else {
            return Vec::new();
        };
        let Some(library) = self.library.as_ref() else {
            return Vec::new();
        };
        book.recent_games(key, limit)
            .into_iter()
            .filter_map(|game| {
                let indexed = library.get(game.index)?;
                Some(PrepRecentRow {
                    index: game.index,
                    title: format!("{} – {}", indexed.white(), indexed.black()),
                    subtitle: format!(
                        "{} · as {} · {} · {}",
                        short_date(game.date),
                        color_name(game.color),
                        outcome_label(indexed.result(), game.color),
                        indexed.event()
                    ),
                })
            })
            .collect()
    }

    fn prep_tree(&mut self, ui: &mut Ui, prep: &PrepSession, pos: &shakmaty::Chess, key: u64) {
        let mut exit = false;
        ui.horizontal(|ui| {
            ui.heading("Player tree");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Exit prep").clicked() {
                    exit = true;
                }
            });
        });
        ui.label(
            RichText::new(format!(
                "vs {} · you play {}",
                prep.player_name,
                color_name(prep.our_color)
            ))
            .weak(),
        );
        ui.separator();
        if exit {
            self.exit_prep();
            return;
        }

        let our_turn = pos.turn() != prep.opponent_color();
        let prediction_data = if our_turn {
            None
        } else {
            self.prep_prediction_data(prep, pos, key)
        };
        let line_rows = if our_turn {
            self.prep_line_rows(prep)
        } else {
            Vec::new()
        };
        let recent_rows = self.prep_recent_rows(prep, key, 50);
        let book_loaded = self
            .player_book
            .as_ref()
            .is_some_and(|book| book.player_key() == prep.player_key);
        let book_loading = self.player_book_job.as_deref() == Some(prep.player_key.as_str());
        let book_status = self.player_book_status.clone();
        let reference_status = self.reference_status.clone();
        let index = self.index.as_ref();
        let index_status = self.index_status.clone();
        let index_building = self.index_rx.is_some();
        let index_job = self
            .index_job
            .as_ref()
            .map(|path| path.display().to_string());
        let mut recent_limit = self.prep_recent_limit.clamp(1, 50);
        let mut clicked = None;
        let mut preview = None;

        egui::ScrollArea::vertical()
            .id_salt("prep-tree-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if our_turn {
                    ui.add_space(2.0);
                    ui.label(RichText::new("It is your move").strong());
                    ui.label(
                        RichText::new(format!(
                            "{}'s moves from earlier in this line stay visible below.",
                            prep.player_name
                        ))
                        .weak()
                        .small(),
                    );
                    ui.add_space(4.0);
                    if line_rows.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "No moves by {} yet in this line.",
                                prep.player_name
                            ))
                            .weak()
                            .italics(),
                        );
                    } else {
                        egui::Grid::new("prep-line-grid")
                            .num_columns(4)
                            .striped(true)
                            .spacing([10.0, 5.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Move").strong());
                                ui.label(RichText::new("Date").strong());
                                ui.label(RichText::new("Tree score").strong())
                                    .on_hover_text("White's score in database games where this move was played.");
                                ui.label(RichText::new("Predicted").strong())
                                    .on_hover_text("Likelihood assigned before this move was chosen.");
                                ui.end_row();

                                for row in &line_rows {
                                    ui.label(RichText::new(&row.label).monospace());
                                    ui.label(
                                        RichText::new(if row.date == 0 {
                                            "—".to_string()
                                        } else {
                                            pgn_date(row.date)
                                        })
                                        .small(),
                                    );
                                    ui.label(
                                        row.tree_score
                                            .map(|score| format!("{:.0}%", score * 100.0))
                                            .unwrap_or_else(|| "—".to_string()),
                                    );
                                    ui.label(
                                        row.likelihood
                                            .map(|probability| {
                                                match (row.likelihood_low, row.likelihood_high) {
                                                    (Some(low), Some(high)) => format!(
                                                        "{:.0}% [{:.0}–{:.0}]",
                                                        probability * 100.0,
                                                        low * 100.0,
                                                        high * 100.0
                                                    ),
                                                    _ => format!("{:.0}%", probability * 100.0),
                                                }
                                            })
                                            .unwrap_or_else(|| "—".to_string()),
                                    );
                                    ui.end_row();
                                }
                            });
                    }
                } else if let Some(data) = &prediction_data {
                    let rows = &data.rows;
                    let stats = &data.stats;
                    let prediction = &data.prediction;
                    let warning = &data.warning;
                    if let Some(stats) = stats {
                        ui.horizontal_wrapped(|ui| {
                            let games_here = if stats.games == 1 {
                                "1 player game here".to_string()
                            } else {
                                format!("{} player games here", stats.games)
                            };
                            ui.label(RichText::new(games_here).strong());
                            ui.label(
                                RichText::new(format!(
                                    "W {} · D {} · L {} · score {:.0}%",
                                    stats.wins,
                                    stats.draws,
                                    stats.losses,
                                    stats.score() * 100.0
                                ))
                                .weak(),
                            );
                            if stats.recent_games > 0 {
                                ui.label(
                                    RichText::new(format!("{} recent", stats.recent_games))
                                        .weak()
                                        .small(),
                                );
                            }
                        });
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "No direct games by {} reached this position.",
                                prep.player_name
                            ))
                            .weak(),
                        );
                    }
                    let prior_label = match prediction.prior_source {
                        players::PriorSource::Reference => {
                            format!("reference prior ({} games)", games_label(prediction.prior_games as usize))
                        }
                        players::PriorSource::CurrentDatabase => format!(
                            "current-database prior ({} games)",
                            games_label(prediction.prior_games as usize)
                        ),
                    };
                    ui.label(
                        RichText::new(format!(
                            "Recency-weighted Bayes · {} player games · {} in the last 2 years · {}",
                            prediction.player_games, prediction.recent_games, prior_label
                        ))
                        .weak()
                        .small(),
                    )
                    .on_hover_text(
                        "Posterior mean with an approximate 90% credible interval. The model uses exponential time decay (18-month half-life), a sample-size-shrunk Dirichlet background prior, and a two-year recent-window mixture. Probability is reserved for legal moves that are not in the data.",
                    );
                    if let Some(warning) = warning {
                        ui.label(RichText::new(warning).small().color(Color32::from_rgb(
                            0xD9, 0xA6, 0x6A,
                        )));
                    }

                    if let Some(top) = rows.first() {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Predicted:").strong());
                            ui.label(
                                RichText::new(domain::figurine_san(&top.san))
                                    .monospace()
                                    .size(18.0)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{:.0}% · 90% CI {:.0}–{:.0}%",
                                    top.probability * 100.0,
                                    top.probability_low * 100.0,
                                    top.probability_high * 100.0
                                ))
                                .strong()
                                .color(Color32::from_rgb(0x9F, 0xD0, 0xA8)),
                            );
                            ui.label(
                                RichText::new(prediction.confidence_label())
                                    .weak()
                                    .small(),
                            );
                            if top.player_games == 0 {
                                ui.label(RichText::new("prior only").weak().small());
                            }
                        });
                    }

                    if rows.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("No move observations to predict from.").weak());
                    } else {
                        ui.add_space(4.0);
                        for row in rows {
                            ui.horizontal(|ui| {
                                let mut text = RichText::new(domain::figurine_san(&row.san))
                                    .monospace()
                                    .strong();
                                if row.player_games == 0 {
                                    text = text.weak();
                                }
                                if ui
                                    .selectable_label(false, text)
                                    .on_hover_text("Play this response")
                                    .clicked()
                                {
                                    clicked = Some(row.mv);
                                }
                                probability_bar(
                                    ui,
                                    row.probability,
                                    row.probability_low,
                                    row.probability_high,
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(4.0);
                                let player = if row.player_games == 0 {
                                    "player: no direct games".to_string()
                                } else {
                                    format!(
                                        "player: {} · {} recent · last {}",
                                        games_label(row.player_games as usize),
                                        row.recent_games,
                                        short_date(row.last_date)
                                    )
                                };
                                ui.label(RichText::new(player).small().weak());
                                let (trend, color) = trend_label(row.trend_pp);
                                ui.label(RichText::new(trend).small().color(color));
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(4.0);
                                let tree = row.tree.map_or_else(
                                    || "tree: no database games".to_string(),
                                    |stat| {
                                        let elo = stat
                                            .elo_avg()
                                            .map(|elo| format!("{elo:.0}"))
                                            .unwrap_or_else(|| "—".to_string());
                                        format!(
                                            "tree: {} · score {:.0}% · W/D/B {}/{}/{} · Elo {} · {}",
                                            games_label(stat.games as usize),
                                            stat.score_white() * 100.0,
                                            stat.white,
                                            stat.draw,
                                            stat.black,
                                            elo,
                                            date_range(stat.first_date, stat.last_date)
                                        )
                                    },
                                );
                                ui.label(RichText::new(tree).small().weak());
                                ui.label(
                                    RichText::new(format!(
                                        "· model prior {}",
                                        games_label(row.prior_games as usize)
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                            ui.add_space(4.0);
                            ui.separator();
                        }
                    }
                    if prediction.unseen_probability > 0.001 {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "Other legal moves not in the data: {:.1}% ({} of {} legal moves)",
                                prediction.unseen_probability * 100.0,
                                prediction.unseen_moves,
                                prediction.legal_moves
                            ))
                            .weak()
                            .small(),
                        )
                        .on_hover_text(
                            "Residual probability reserved for moves absent from the player book and the background prior. This is why a small sample never reaches 100%.",
                        );
                    }
                } else {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&book_status).weak());
                    if book_loading {
                        ui.spinner();
                    }
                }

                ui.add_space(8.0);
                ui.label(RichText::new(&reference_status).small().weak());
                egui::CollapsingHeader::new(RichText::new("Opening tree").strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        if index.is_some() {
                            clicked = opening_tree_table(
                                ui,
                                index,
                                key,
                                pos,
                                "No indexed game reached this position.",
                            )
                            .or(clicked);
                        } else {
                            ui.label(RichText::new(&index_status).weak());
                            if index_building {
                                ui.label(
                                    RichText::new(
                                        "Building the position index in the background.",
                                    )
                                    .weak()
                                    .small(),
                                );
                            } else if let Some(job) = &index_job {
                                ui.label(RichText::new(job).weak().small());
                            }
                        }
                    });

                ui.add_space(4.0);
                egui::CollapsingHeader::new(RichText::new("Recent games").strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Show last");
                            ui.add(
                                egui::DragValue::new(&mut recent_limit)
                                    .range(1..=50)
                                    .speed(1.0),
                            );
                            ui.label("games");
                        });
                        ui.label(
                            RichText::new(format!(
                                "Newest first · from {}'s games in this database",
                                prep.player_name
                            ))
                            .weak()
                            .small(),
                        );
                        ui.add_space(3.0);
                        if !book_loaded {
                            ui.label(RichText::new(&book_status).weak());
                            if book_loading {
                                ui.spinner();
                            }
                        } else if recent_rows.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "No {} game reached this position.",
                                    prep.player_name
                                ))
                                .weak()
                                .italics(),
                            );
                        } else {
                            let width = (ui.available_width() - 4.0).max(100.0);
                            for row in recent_rows.iter().take(recent_limit) {
                                if game_row(ui, false, &row.title, &row.subtitle, width, 40.0) {
                                    preview = Some(row.index);
                                }
                            }
                            if recent_rows.len() > recent_limit {
                                ui.label(
                                    RichText::new(format!(
                                        "Showing {} of up to 50 loaded games.",
                                        recent_limit
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        }
                    });

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Probabilities cover moves seen in this player's games and the background prior. Residual mass is shown for other legal moves. Recent games are drawn from the player book.")
                        .weak()
                        .small(),
                );
            });

        self.prep_recent_limit = recent_limit;
        if let Some(mv) = clicked {
            self.play_move(mv);
        }
        if let Some(index) = preview {
            self.open_recent_preview(index);
        }
    }

    /// Open a database game in read-only preview while retaining the whole
    /// preparation session, including the current node and right-hand tab.
    fn open_recent_preview(&mut self, index: usize) {
        if self.prep.is_none() || self.recent_preview.is_some() {
            return;
        }
        let Some(library) = self.library.clone() else {
            return;
        };
        let Some(return_game) = self.current.clone() else {
            return;
        };
        let return_node = self.node;
        let return_tab = self.right_tab;
        match library.load(index) {
            Ok(game) => {
                self.clear_review();
                self.clear_engine_results();
                self.annotation_editor = None;
                self.annotation_focus = false;
                self.recent_preview = Some(RecentPreview {
                    return_game,
                    return_node,
                    return_tab,
                });
                self.current = Some(game);
                self.game_idx = usize::MAX;
                self.dirty = false;
                self.right_tab = RightTab::Moves;
                self.goto(0);
                self.status = format!("Previewing game {} · read only", index + 1);
            }
            Err(err) => {
                self.status = format!("Could not open game {}: {err}", index + 1);
            }
        }
    }

    fn close_recent_preview(&mut self) {
        let Some(preview) = self.recent_preview.take() else {
            return;
        };
        // Review and engine results belong to the preview, not to the
        // preparation game that is about to be restored.
        self.clear_review();
        self.clear_engine_results();
        self.current = Some(preview.return_game);
        self.game_idx = usize::MAX;
        self.dirty = false;
        self.right_tab = preview.return_tab;
        self.annotation_editor = None;
        self.annotation_focus = false;
        self.goto(preview.return_node);
        self.status = if let Some(prep) = &self.prep {
            format!("Returned to preparation against {}", prep.player_name)
        } else {
            "Closed game preview.".to_string()
        };
    }

    fn preview_content(&mut self, ui: &mut Ui) {
        let Some(game) = self.current.clone() else {
            ui.heading("No game");
            return;
        };
        let node = self.node.min(game.nodes.len().saturating_sub(1));
        let mut close = false;
        let mut clicked = None;

        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent game").strong().color(GOLD));
            ui.label(RichText::new("read only").small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Close preview").clicked() {
                    close = true;
                }
            });
        });
        ui.label(
            RichText::new("Preparation is paused; closing returns to the same position.")
                .weak()
                .small(),
        );
        ui.add_space(6.0);
        game_header_card(ui, &game, None);
        ui.add_space(8.0);

        let lines = domain::move_lines(&game);
        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(10.0)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Moves").strong());
                    ui.label(
                        RichText::new(format!("{} plies", game.mainline_len()))
                            .weak()
                            .small(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("← / → to step").small().weak());
                    });
                });
                ui.add_space(2.0);

                let root_comment = game.nodes[0].comment.trim();
                if !root_comment.is_empty() {
                    ui.label(
                        RichText::new(format!("“{}”", comment_preview(root_comment, 72)))
                            .italics()
                            .small()
                            .color(COMMENT_TEXT),
                    )
                    .on_hover_text(root_comment);
                }

                egui::ScrollArea::vertical()
                    .id_salt("preview-moves")
                    .max_height((ui.available_height() - 8.0).max(100.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if lines.is_empty() {
                            ui.label(RichText::new("No moves in this game.").weak().italics());
                            return;
                        }
                        ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                        for line in &lines {
                            render_readonly_move_line(ui, line, &game, node, &mut clicked);
                        }
                    });
            });

        if close {
            self.close_recent_preview();
        } else if let Some(node) = clicked {
            self.goto(node);
        }
    }

    fn play_move(&mut self, mv: Move) {
        if self.recent_preview.is_some() {
            return;
        }
        let uci = UciMove::from_move(mv, CastlingMode::Standard).to_string();
        let existing = {
            let Some(game) = self.current.as_ref() else {
                return;
            };
            game.nodes[self.node]
                .children
                .iter()
                .copied()
                .find(|&child| {
                    game.nodes[child]
                        .mv
                        .as_ref()
                        .is_some_and(|ply| ply.uci == uci)
                })
        };
        if let Some(child) = existing {
            self.goto(child);
            return;
        }

        let Some(game) = self.current.as_mut() else {
            return;
        };
        let id = game.add_move(self.node, mv);
        self.dirty = true;
        self.goto(id);
    }

    fn play_uci(&mut self, uci: &str) {
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            return;
        };
        let Ok(uci) = UciMove::from_ascii(uci.as_bytes()) else {
            return;
        };
        let Ok(mv) = uci.to_move(&pos) else {
            return;
        };
        self.play_move(mv);
    }

    fn bottom_bar(&mut self, root: &mut Ui) {
        egui::Panel::bottom("nav").show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("|<").on_hover_text("Start (Home)").clicked() {
                    self.goto(0);
                }
                if ui.button("<").on_hover_text("Previous move (←)").clicked() {
                    self.prev_move();
                }
                if ui.button(">").on_hover_text("Next move (→)").clicked() {
                    self.next_move();
                }
                if ui.button(">|").on_hover_text("End (End)").clicked() {
                    self.go_end();
                }
                ui.separator();
                if ui.button("Flip").on_hover_text("Flip board (F)").clicked() {
                    self.orientation = self.orientation.other();
                }
                if self.current.as_ref().is_some_and(|game| game.has_clocks()) {
                    ui.toggle_value(&mut self.show_clocks, "Clocks")
                        .on_hover_text("Show PGN clock times beside the board");
                }
                if ui.button("Var").on_hover_text("Next variation").clicked() {
                    self.next_variation();
                }

                ui.separator();
                let (ply, total) = match self.game() {
                    Some(g) => (depth_of(g, self.node), g.mainline_len()),
                    None => (0, 0),
                };
                ui.label(RichText::new(format!("move {ply} / {total}")).weak());

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Load").clicked() {
                        let path = std::path::PathBuf::from(self.path_input.trim());
                        if !path.as_os_str().is_empty() {
                            self.load_file(&path);
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("/path/to/file.pgn, database.si4, or database.cbh")
                            .desired_width(340.0),
                    );
                    if ui.button("Open…").clicked()
                        && let Some(path) = open_dialog()
                    {
                        self.load_file(&path);
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn online_dialog(&mut self, ctx: &egui::Context) {
        if !self.online_open {
            return;
        }
        let mut open = self.online_open;
        let mut start = false;
        egui::Window::new("Import online games")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Download public games from an account and open them as a temporary database.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Source");
                    egui::ComboBox::from_id_salt("online-source")
                        .selected_text(self.online_source.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.online_source,
                                OnlineSource::Lichess,
                                "Lichess",
                            );
                            ui.selectable_value(
                                &mut self.online_source,
                                OnlineSource::ChessCom,
                                "Chess.com",
                            );
                        });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.online_username)
                        .hint_text("username")
                        .desired_width(260.0),
                );
                ui.horizontal(|ui| {
                    ui.label("Maximum games");
                    ui.add(
                        egui::DragValue::new(&mut self.online_max)
                            .range(50..=5000)
                            .speed(50),
                    );
                });
                ui.label(
                    RichText::new(
                        "Lichess exports in one request. Chess.com is assembled from recent monthly archives; at most 36 months are read.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(4.0);
                let running = self.online_rx.is_some();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !running && !self.online_username.trim().is_empty(),
                            egui::Button::new("Import"),
                        )
                        .clicked()
                    {
                        start = true;
                    }
                    if running {
                        ui.spinner();
                    }
                });
                if !self.online_status.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.online_status));
                }
            });
        self.online_open = open;
        if start {
            self.start_online_import();
        }
    }

    fn about(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        egui::Window::new("About Brett")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("A native Linux chess preparation workspace.");
                ui.add_space(6.0);
                ui.label("Piece set: cburnett by Colin M. L. Burnett, CC BY-SA 3.0.");
                ui.label("Rules and SAN: shakmaty. PGN: pgn-reader.");
                ui.label("SCID and classic CBH readers: independent native implementations.");
                ui.label("Chess symbols: DejaVu Sans, Bitstream Vera / public-domain licence.");
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    self.show_about = false;
                }
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_index();
        self.poll_player_book();
        self.poll_reference();
        self.poll_online();
        self.poll_cloud();
        self.poll_engine();
        self.poll_review();
        self.maybe_engine_analysis();
        if self.index_rx.is_some()
            || self.player_book_rx.is_some()
            || self.reference_rx.is_some()
            || self.online_rx.is_some()
            || self.cloud_rx.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.engine_searching || (self.engine.is_some() && !self.engine_ready) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.review.as_ref().is_some_and(|review| review.running) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.first() {
            self.load_file(path);
        }

        self.handle_keys(&ctx);
        self.menu_bar(ui);
        self.database_panel(ui);
        self.moves_panel(ui);
        self.bottom_bar(ui);

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let avail = ui.available_size();
            let side = (avail.x.min(avail.y - 64.0) - 24.0).max(120.0);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space(6.0);
                let board_meta = self.game().map(|game| {
                    let node = &game.nodes[self.node];
                    let top_color = self.orientation.other();
                    let bottom_color = self.orientation;
                    let show_clocks = self.show_clocks && game.has_clocks();
                    let badge = |color: Color| PlayerBadge {
                        name: if color == Color::White {
                            game.white().to_string()
                        } else {
                            game.black().to_string()
                        },
                        rating: player_rating(game, color),
                        clock: if show_clocks {
                            game.clock_for(self.node, color)
                        } else {
                            None
                        },
                        color,
                    };
                    BoardMeta {
                        pos: node.pos.clone(),
                        last_move: domain::last_move_squares(node),
                        top: badge(top_color),
                        bottom: badge(bottom_color),
                        show_clocks,
                    }
                });
                if let Some(meta) = board_meta {
                    player_strip(ui, side, &meta.top, meta.show_clocks);
                    ui.add_space(2.0);
                    let event = board::draw(
                        ui,
                        &self.textures,
                        BoardState {
                            pos: &meta.pos,
                            orientation: self.orientation,
                            last_move: meta.last_move,
                            selected: self.selected,
                            targets: &self.targets,
                            drag_from: self.drag_from,
                        },
                        side,
                    );
                    ui.add_space(2.0);
                    player_strip(ui, side, &meta.bottom, meta.show_clocks);
                    if let Some(event) = event {
                        self.handle_board_event(event);
                    }
                } else {
                    ui.add_space(40.0);
                    ui.label(RichText::new("Drop a chess database here").heading());
                    ui.label(
                        RichText::new(
                            "PGN, SCID4/3, or classic ChessBase · or use File → Open database…",
                        )
                        .weak(),
                    );
                }
            });
        });

        self.about(&ctx);
        self.online_dialog(&ctx);
    }
}

/// Player name, rating and (optionally) clock above/below the board.
fn player_strip(ui: &mut Ui, side: f32, player: &PlayerBadge, show_clock: bool) {
    let (rect, response) = ui.allocate_exact_size(vec2(side, 27.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, Color32::from_rgb(0x1F, 0x22, 0x25));

    let dot = pos2(rect.left() + 12.0, rect.center().y);
    let fill = match player.color {
        Color::White => Color32::from_gray(235),
        Color::Black => Color32::from_gray(35),
    };
    painter.circle_filled(dot, 4.5, fill);
    painter.circle_stroke(dot, 4.5, Stroke::new(1.0, Color32::from_gray(135)));

    let mut label = player.name.clone();
    if let Some(rating) = &player.rating {
        label.push_str(&format!("  ({rating})"));
    }
    let name_clip = egui::Rect::from_min_max(
        rect.min,
        pos2(
            rect.right() - if show_clock { 104.0 } else { 8.0 },
            rect.bottom(),
        ),
    );
    let name_painter = ui.painter_at(name_clip);
    let name_galley =
        name_painter.layout_no_wrap(label, FontId::proportional(14.0), Color32::from_gray(226));
    name_painter.galley(
        pos2(
            rect.left() + 23.0,
            rect.center().y - name_galley.size().y / 2.0,
        ),
        name_galley,
        Color32::from_gray(226),
    );

    if show_clock {
        let text = player
            .clock
            .map(Clock::display)
            .unwrap_or_else(|| "--:--".to_string());
        let color = if player.clock.is_some() {
            Color32::from_rgb(0xE8, 0xE4, 0xD8)
        } else {
            Color32::from_gray(120)
        };
        let clock_galley = painter.layout_no_wrap(text, FontId::monospace(14.0), color);
        let clock_rect = egui::Rect::from_min_size(
            pos2(
                rect.right() - clock_galley.size().x - 10.0,
                rect.center().y - clock_galley.size().y / 2.0,
            ),
            clock_galley.size(),
        );
        painter.rect_filled(
            clock_rect.expand2(vec2(7.0, 3.0)),
            4.0,
            Color32::from_rgb(0x2A, 0x2E, 0x32),
        );
        painter.galley(clock_rect.min, clock_galley, color);
    }

    response.on_hover_text(format!(
        "{} · {}",
        color_name(player.color),
        player.rating.as_deref().unwrap_or("no rating")
    ));
}

/// Convert an engine PV from UCI to a short figurine SAN line.
fn pv_san(pos: &Chess, pv: &[String], limit: usize) -> String {
    let mut pos = pos.clone();
    let mut out = Vec::new();
    for text in pv.iter().take(limit) {
        let Ok(uci) = UciMove::from_ascii(text.as_bytes()) else {
            break;
        };
        let Ok(mv) = uci.to_move(&pos) else {
            break;
        };
        let san = SanPlus::from_move(pos.clone(), mv).to_string();
        out.push(domain::figurine_san(&san));
        let Ok(next) = pos.clone().play(mv) else {
            break;
        };
        pos = next;
    }
    out.join(" ")
}

/// Rating shown beside a player's name. Elo wins over DWZ when both exist.
fn player_rating(game: &Game, color: Color) -> Option<String> {
    let (elo_key, dwz_key) = match color {
        Color::White => ("WhiteElo", "WhiteDWZ"),
        Color::Black => ("BlackElo", "BlackDWZ"),
    };
    if let Some(elo) = game
        .header(elo_key)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
    {
        return Some(elo.to_string());
    }
    game.header(dwz_key)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .map(|dwz| format!("DWZ {dwz}"))
}

fn engine_score_label(score: Option<Score>, turn: Color) -> String {
    match score {
        Some(Score::Cp(cp)) => {
            let white = if turn == Color::White { cp } else { -cp };
            format!("{:+.2}", white as f64 / 100.0)
        }
        Some(Score::Mate(mate)) => {
            let white = if turn == Color::White { mate } else { -mate };
            format!("#{white}")
        }
        None => "—".to_string(),
    }
}

fn quality_nag(quality: Quality) -> Option<u8> {
    match quality {
        Quality::Best => Some(3),      // ‼ / very good
        Quality::Excellent => Some(1), // !
        Quality::Good => None,
        Quality::Inaccuracy => Some(6), // ?!
        Quality::Mistake => Some(2),    // ?
        Quality::Blunder => Some(4),    // ??
        Quality::Forced | Quality::Unknown => None,
    }
}

fn quality_color(quality: Quality) -> Color32 {
    match quality {
        Quality::Best => Color32::from_rgb(0x7E, 0xD3, 0x8A),
        Quality::Excellent => Color32::from_rgb(0x9C, 0xD3, 0x93),
        Quality::Good => Color32::from_rgb(0xB8, 0xC4, 0xA8),
        Quality::Inaccuracy => Color32::from_rgb(0xE8, 0xC4, 0x6A),
        Quality::Mistake => Color32::from_rgb(0xE8, 0x99, 0x5A),
        Quality::Blunder => Color32::from_rgb(0xE0, 0x6C, 0x6C),
        Quality::Forced => Color32::from_gray(155),
        Quality::Unknown => Color32::from_gray(135),
    }
}

/// Small white-perspective evaluation chart. Mates are clamped to the top and
/// bottom so one forced mate does not flatten the rest of the game.
fn review_graph(ui: &mut Ui, review: &ReviewState) {
    let width = ui.available_width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 96.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, Color32::from_rgb(0x18, 0x1A, 0x1C));
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let middle = rect.center().y;
    painter.line_segment(
        [
            pos2(rect.left() + 6.0, middle),
            pos2(rect.right() - 6.0, middle),
        ],
        Stroke::new(1.0, Color32::from_gray(62)),
    );
    painter.text(
        pos2(rect.left() + 7.0, rect.top() + 4.0),
        Align2::LEFT_TOP,
        "+8",
        FontId::monospace(9.0),
        Color32::from_gray(95),
    );
    painter.text(
        pos2(rect.left() + 7.0, rect.bottom() - 4.0),
        Align2::LEFT_BOTTOM,
        "−8",
        FontId::monospace(9.0),
        Color32::from_gray(95),
    );

    let points = review.eval_points();
    if points.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Waiting for evaluations…",
            FontId::proportional(11.0),
            Color32::from_gray(110),
        );
        return;
    }

    let denominator = review.total().max(2).saturating_sub(1) as f32;
    let scale = (rect.height() / 2.0 - 10.0) / 800.0;
    let position = |index: usize, cp: f64| {
        pos2(
            rect.left() + 8.0 + (index as f32 / denominator) * (rect.width() - 16.0),
            middle - (cp.clamp(-800.0, 800.0) as f32) * scale,
        )
    };
    let mut previous = None;
    for &(index, cp) in &points {
        let point = position(index, cp);
        if let Some(previous) = previous {
            painter.line_segment(
                [previous, point],
                Stroke::new(1.5, Color32::from_rgb(0xC9, 0xB4, 0x7A)),
            );
        }
        previous = Some(point);
    }
    if let Some(&(index, cp)) = points.last() {
        painter.circle_filled(position(index, cp), 3.0, GOLD);
    }
}

fn engine_eval_color(score: Option<Score>, turn: Color) -> Color32 {
    match score {
        Some(Score::Mate(_)) => GOLD,
        Some(Score::Cp(cp)) => {
            let white = if turn == Color::White { cp } else { -cp };
            if white > 20 {
                Color32::from_rgb(0x9E, 0xD8, 0xA8)
            } else if white < -20 {
                Color32::from_rgb(0xE0, 0xA0, 0xA0)
            } else {
                Color32::from_gray(190)
            }
        }
        None => Color32::from_gray(150),
    }
}

const COMMON_PGN_TAGS: &[&str] = &[
    "Event",
    "Site",
    "Date",
    "Round",
    "White",
    "Black",
    "Result",
    "WhiteElo",
    "BlackElo",
    "WhiteDWZ",
    "BlackDWZ",
    "ECO",
    "Opening",
    "Variation",
    "TimeControl",
    "Termination",
    "Annotator",
    "EventDate",
    "EventType",
    "FEN",
    "SetUp",
];

/// Editable PGN tag list. Tag names are fixed labels; only the value can be
/// changed. A field can be removed with its delete button and added back from
/// the common-tag menu.
fn game_tags_card(
    ui: &mut Ui,
    game: &Game,
    tag_update: &mut Option<(usize, String)>,
    tag_delete: &mut Option<usize>,
    tag_add: &mut Option<&'static str>,
) {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(10.0)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(10.0)
        .show(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new("PGN tags").strong())
                .default_open(true)
                .show(ui, |ui| {
                    if game.headers.is_empty() {
                        ui.label(RichText::new("No tags.").weak().italics());
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("pgn-tags-scroll")
                        .max_height(230.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("pgn-tags-grid")
                                .num_columns(3)
                                .spacing([6.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for (index, (name, value)) in game.headers.iter().enumerate() {
                                        ui.label(
                                            RichText::new(name)
                                                .monospace()
                                                .small()
                                                .color(Color32::from_gray(175)),
                                        );
                                        let mut edited = value.clone();
                                        let response = ui.add(
                                            egui::TextEdit::singleline(&mut edited)
                                                .id_salt(("pgn-tag", index))
                                                .desired_width(205.0)
                                                .hint_text("value"),
                                        );
                                        if response.changed() {
                                            *tag_update = Some((index, edited));
                                        }
                                        if ui
                                            .small_button("×")
                                            .on_hover_text("Remove this tag")
                                            .clicked()
                                        {
                                            *tag_delete = Some(index);
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(3.0);
                    ui.menu_button("+ Add tag", |ui| {
                        let mut any = false;
                        for &tag in COMMON_PGN_TAGS {
                            if game.header(tag).is_none() {
                                any = true;
                                if ui.button(tag).clicked() {
                                    *tag_add = Some(tag);
                                    ui.close();
                                }
                            }
                        }
                        if !any {
                            ui.label(RichText::new("All common tags are present.").weak());
                        }
                    });
                });
        });
}

/// Header card above the PGN move list.
fn game_header_card(ui: &mut Ui, game: &Game, prep: Option<&PrepSession>) {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(10.0)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(10.0)
        .show(ui, |ui| {
            if let Some(prep) = prep {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PREPARATION").small().strong().color(GOLD));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("you play {}", color_name(prep.our_color)))
                                .small()
                                .weak(),
                        );
                    });
                });
                ui.label(RichText::new(format!("vs {}", prep.player_name)).heading());
                ui.label(
                    RichText::new("Make a move on the board; the opponent tree will follow.")
                        .weak()
                        .small(),
                );
                return;
            }

            ui.horizontal(|ui| {
                ui.label(RichText::new(game.event()).small().strong().color(GOLD));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    result_badge(ui, game.result());
                });
            });
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(game.white()).strong().size(16.0));
                if let Some(rating) = player_rating(game, Color::White) {
                    ui.label(RichText::new(format!("({rating})")).small().weak());
                }
                ui.label(RichText::new("vs").weak().small());
                ui.label(RichText::new(game.black()).strong().size(16.0));
                if let Some(rating) = player_rating(game, Color::Black) {
                    ui.label(RichText::new(format!("({rating})")).small().weak());
                }
            });
            let mut details = vec![game.date().to_string()];
            if let Some(round) = game
                .header("Round")
                .filter(|round| !round.is_empty() && *round != "?")
            {
                details.push(format!("Round {round}"));
            }
            if let Some(eco) = game.header("ECO").filter(|eco| !eco.is_empty()) {
                details.push(eco.to_string());
            }
            ui.label(RichText::new(details.join(" · ")).weak().small());
        });
}

fn result_badge(ui: &mut Ui, result: &str) {
    let (label, fill, text) = match result {
        "1-0" => (
            "1–0",
            Color32::from_rgb(0x32, 0x43, 0x34),
            Color32::from_rgb(0xBF, 0xDF, 0xC4),
        ),
        "0-1" => (
            "0–1",
            Color32::from_rgb(0x43, 0x33, 0x33),
            Color32::from_rgb(0xE0, 0xC0, 0xC0),
        ),
        "1/2-1/2" => (
            "½–½",
            Color32::from_rgb(0x39, 0x3B, 0x3E),
            Color32::from_rgb(0xD0, 0xD2, 0xD4),
        ),
        other => (
            other,
            Color32::from_rgb(0x33, 0x35, 0x38),
            Color32::from_gray(190),
        ),
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(label).small().strong().color(text));
        });
}

fn render_readonly_move_line(
    ui: &mut Ui,
    line: &domain::MoveLine,
    game: &Game,
    selected: usize,
    clicked: &mut Option<usize>,
) {
    if line.depth > 0 {
        ui.horizontal(|ui| {
            ui.add_space(8.0 * line.depth as f32);
            egui::Frame::new()
                .fill(VARIATION_BG)
                .corner_radius(5.0)
                .inner_margin(5.0)
                .show(ui, |ui| {
                    render_readonly_move_tokens(ui, line, game, selected, clicked);
                });
        });
    } else {
        render_readonly_move_tokens(ui, line, game, selected, clicked);
    }
}

fn render_readonly_move_tokens(
    ui: &mut Ui,
    line: &domain::MoveLine,
    game: &Game,
    selected: usize,
    clicked: &mut Option<usize>,
) {
    ui.horizontal_wrapped(|ui| {
        for token in &line.tokens {
            match token {
                Token::OpenParen => {
                    ui.label(RichText::new("(").weak().small());
                }
                Token::CloseParen => {
                    ui.label(RichText::new(")").weak().small());
                }
                Token::Move {
                    node,
                    number,
                    color,
                    san,
                    line_start,
                } => {
                    let prefix = match color {
                        Color::White => format!("{number}."),
                        Color::Black if *line_start => format!("{number}…"),
                        Color::Black => String::new(),
                    };
                    if !prefix.is_empty() {
                        ui.label(
                            RichText::new(prefix)
                                .monospace()
                                .small()
                                .color(Color32::from_gray(135)),
                        );
                    }
                    let response = move_chip(ui, *node == selected, san, line.depth > 0);
                    if response.clicked() {
                        *clicked = Some(*node);
                    }

                    let symbols = nag_symbols(&game.nodes[*node].nags);
                    if !symbols.is_empty() {
                        ui.label(RichText::new(symbols).small().strong().color(GOLD));
                    }

                    let comment = game.nodes[*node].comment.trim();
                    if !comment.is_empty() {
                        ui.label(
                            RichText::new(format!("“{}”", comment_preview(comment, 48)))
                                .italics()
                                .small()
                                .color(COMMENT_TEXT),
                        )
                        .on_hover_text(comment);
                    }
                }
            }
        }
    });
}

fn render_move_line(
    ui: &mut Ui,
    line: &domain::MoveLine,
    game: &Game,
    selected: usize,
    clicked: &mut Option<usize>,
    open_editor: &mut Option<usize>,
    nag_action: &mut Option<(usize, Option<u8>)>,
) {
    if line.depth > 0 {
        ui.horizontal(|ui| {
            ui.add_space(8.0 * line.depth as f32);
            egui::Frame::new()
                .fill(VARIATION_BG)
                .corner_radius(5.0)
                .inner_margin(5.0)
                .show(ui, |ui| {
                    render_move_tokens(ui, line, game, selected, clicked, open_editor, nag_action);
                });
        });
    } else {
        render_move_tokens(ui, line, game, selected, clicked, open_editor, nag_action);
    }
}

fn render_move_tokens(
    ui: &mut Ui,
    line: &domain::MoveLine,
    game: &Game,
    selected: usize,
    clicked: &mut Option<usize>,
    open_editor: &mut Option<usize>,
    nag_action: &mut Option<(usize, Option<u8>)>,
) {
    ui.horizontal_wrapped(|ui| {
        for token in &line.tokens {
            match token {
                Token::OpenParen => {
                    ui.label(RichText::new("(").weak().small());
                }
                Token::CloseParen => {
                    ui.label(RichText::new(")").weak().small());
                }
                Token::Move {
                    node,
                    number,
                    color,
                    san,
                    line_start,
                } => {
                    let prefix = match color {
                        Color::White => format!("{number}."),
                        Color::Black if *line_start => format!("{number}…"),
                        Color::Black => String::new(),
                    };
                    if !prefix.is_empty() {
                        ui.label(
                            RichText::new(prefix)
                                .monospace()
                                .small()
                                .color(Color32::from_gray(135)),
                        );
                    }
                    let response = move_chip(ui, *node == selected, san, line.depth > 0);
                    if response.clicked() {
                        *clicked = Some(*node);
                    }
                    move_context_menu(&response, *node, game, nag_action, open_editor);

                    let symbols = nag_symbols(&game.nodes[*node].nags);
                    if !symbols.is_empty() {
                        let response = annotation_chip(ui, &symbols, line.depth > 0);
                        if response.clicked() {
                            *open_editor = Some(*node);
                        }
                        move_context_menu(&response, *node, game, nag_action, open_editor);
                    }

                    let comment = game.nodes[*node].comment.trim();
                    if !comment.is_empty() {
                        let preview = comment_preview(comment, 48);
                        let response = ui.add(
                            egui::Label::new(
                                RichText::new(format!("“{preview}”"))
                                    .italics()
                                    .small()
                                    .color(COMMENT_TEXT),
                            )
                            .sense(Sense::click()),
                        );
                        if response.clicked() {
                            *open_editor = Some(*node);
                        }
                        response
                            .on_hover_text("Click to edit this comment")
                            .on_hover_text(comment);
                    }
                }
            }
        }
    });
}

fn move_chip(ui: &mut Ui, selected: bool, san: &str, muted: bool) -> egui::Response {
    let text_color = if selected {
        Color32::WHITE
    } else if muted {
        Color32::from_gray(180)
    } else {
        Color32::from_gray(225)
    };
    let galley = ui.painter().layout_no_wrap(
        domain::figurine_san(san),
        FontId::monospace(13.5),
        text_color,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(12.0, 6.0), Sense::click());
    let fill = if selected {
        GOLD_SOFT
    } else if response.hovered() {
        Color32::from_gray(43)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, fill);
    }
    if selected {
        ui.painter()
            .rect_stroke(rect, 4.0, Stroke::new(1.0, GOLD), StrokeKind::Inside);
    }
    ui.painter()
        .galley(rect.min + vec2(6.0, 3.0), galley, text_color);
    response.on_hover_text("Left-click to jump · right-click to annotate")
}

fn annotation_chip(ui: &mut Ui, symbols: &str, muted: bool) -> egui::Response {
    let color = if muted {
        Color32::from_rgb(0xD2, 0xBC, 0x86)
    } else {
        GOLD
    };
    let galley =
        ui.painter()
            .layout_no_wrap(symbols.to_string(), FontId::proportional(11.5), color);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(9.0, 3.0), Sense::click());
    let fill = if response.hovered() {
        GOLD_SOFT
    } else {
        Color32::from_rgb(0x33, 0x30, 0x23)
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    ui.painter()
        .galley(rect.min + vec2(4.5, 1.5), galley, color);
    response.on_hover_text("Click to edit annotations · right-click for the palette")
}

fn move_context_menu(
    response: &egui::Response,
    node: usize,
    game: &Game,
    nag_action: &mut Option<(usize, Option<u8>)>,
    open_editor: &mut Option<usize>,
) {
    response.context_menu(|ui| {
        ui.set_min_width(260.0);
        let san = game.nodes[node]
            .mv
            .as_ref()
            .map(|ply| domain::figurine_san(&ply.san))
            .unwrap_or_else(|| "Start position".to_string());
        ui.label(RichText::new(format!("Annotate {san}")).strong());
        ui.separator();
        if node > 0 {
            if let Some(nag) = nag_palette(ui, &game.nodes[node].nags, true) {
                *nag_action = Some((node, Some(nag)));
                ui.close();
            }
        } else {
            ui.label(RichText::new("Annotations belong to moves.").weak());
        }
        ui.separator();
        if ui.button("Edit comment and annotations…").clicked() {
            *open_editor = Some(node);
            ui.close();
        }
        if ui.button("Clear annotations").clicked() {
            *nag_action = Some((node, None));
            ui.close();
        }
    });
}

fn nag_palette(ui: &mut Ui, current: &[u8], compact: bool) -> Option<u8> {
    let mut clicked = None;
    for &(group, choices) in domain::NAG_GROUPS {
        ui.add_space(if compact { 1.0 } else { 2.0 });
        ui.label(
            RichText::new(group)
                .small()
                .strong()
                .color(Color32::from_gray(160)),
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for &choice in choices {
                let active = current.contains(&choice.code);
                let button = egui::Button::new(
                    RichText::new(choice.symbol)
                        .size(if compact { 12.0 } else { 14.0 })
                        .strong()
                        .color(if active {
                            Color32::from_rgb(0x19, 0x1B, 0x1D)
                        } else {
                            GOLD
                        }),
                )
                .fill(if active {
                    GOLD
                } else {
                    Color32::from_rgb(0x2A, 0x2E, 0x32)
                })
                .stroke(Stroke::new(1.0, if active { GOLD } else { BORDER }))
                .corner_radius(4.0)
                .min_size(vec2(
                    if compact { 26.0 } else { 32.0 },
                    if compact { 20.0 } else { 25.0 },
                ));
                if ui.add(button).on_hover_text(choice.label).clicked() {
                    clicked = Some(choice.code);
                }
            }
        });
    }
    clicked
}

fn nag_symbols(nags: &[u8]) -> String {
    let mut out = String::new();
    for &nag in nags {
        if let Some(symbol) = domain::nag_symbol(nag) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(symbol);
        }
    }
    out
}

fn nag_conflicts(existing: u8, new: u8) -> bool {
    fn group(nag: u8) -> u8 {
        match nag {
            1..=9 => 1,
            10..=21 => 2,
            _ => 3,
        }
    }
    let new_group = group(new);
    new_group < 3 && group(existing) == new_group
}

fn comment_preview(comment: &str, max_chars: usize) -> String {
    let flat = comment.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn depth_of(game: &Game, mut node: usize) -> usize {
    let mut d = 0;
    while let Some(parent) = game.nodes[node].parent {
        node = parent;
        d += 1;
    }
    d
}

fn games_label(n: usize) -> String {
    if n == 1 {
        "1 game".to_string()
    } else {
        format!("{n} games")
    }
}

fn positions_label(n: usize) -> String {
    if n == 1 {
        "1 position".to_string()
    } else {
        format!("{n} positions")
    }
}

fn moves_label(n: usize) -> String {
    if n == 1 {
        "1 move".to_string()
    } else {
        format!("{n} moves")
    }
}

/// Two-line, full-width selectable row for the game list.
fn game_row(
    ui: &mut Ui,
    selected: bool,
    title: &str,
    subtitle: &str,
    width: f32,
    height: f32,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let visuals = ui.visuals();
    let bg = if selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, bg);
    painter.text(
        rect.left_top() + vec2(8.0, 5.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(14.0),
        visuals.strong_text_color(),
    );
    painter.text(
        rect.left_top() + vec2(8.0, 23.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(11.0),
        visuals.weak_text_color(),
    );
    response.clicked()
}

fn fen_string(pos: &shakmaty::Chess) -> String {
    shakmaty::fen::Fen::from_position(pos, shakmaty::EnPassantMode::Legal).to_string()
}

fn format_cp(cp: i32) -> String {
    format!("{:+.2}", cp as f64 / 100.0)
}

fn pv_score(cp: Option<i32>, mate: Option<i32>) -> String {
    if let Some(mate) = mate {
        return format!("M{mate}");
    }
    cp.map(format_cp).unwrap_or_else(|| "—".to_string())
}

fn format_percent(count: u64, total: u64) -> String {
    if total == 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", count as f64 * 100.0 / total as f64)
    }
}

fn open_library(path: &std::path::Path) -> anyhow::Result<Arc<dyn GameLibrary>> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension == "si4" || extension == "si3" {
        ScidLibrary::open(path).map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else if extension == "cbh" {
        CbhLibrary::open(path).map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else {
        PgnLibrary::build(path.to_path_buf())
            .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    }
}

fn index_summary(index: &PositionIndex) -> String {
    format!(
        "{} · {} positions · {:.1}s · {} index",
        games_label(index.game_count()),
        index.position_count(),
        index.build_time().as_secs_f64(),
        bytes_label(index.file_len())
    )
}

fn bytes_label(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    }
}

fn date_range(first: u32, last: u32) -> String {
    let first = first / 10_000;
    let last = last / 10_000;
    match (first, last) {
        (0, 0) => "—".to_string(),
        (0, last) => format!("{last}"),
        (first, 0) => format!("{first}"),
        (first, last) if first == last => format!("{first}"),
        (first, last) => format!("{first}–{last}"),
    }
}

fn color_name(color: Color) -> &'static str {
    match color {
        Color::White => "White",
        Color::Black => "Black",
    }
}

/// Result from the prepared player's perspective.
fn outcome_label(result: &str, color: Color) -> &'static str {
    match (result, color) {
        ("1-0", Color::White) | ("0-1", Color::Black) => "won",
        ("0-1", Color::White) | ("1-0", Color::Black) => "lost",
        ("1/2-1/2", _) => "drew",
        _ => "unfinished",
    }
}

fn prep_matches(app: &App, player_key: &str) -> bool {
    app.prep
        .as_ref()
        .is_some_and(|prep| prep.player_key == player_key)
}

fn pgn_date(date: u32) -> String {
    format!(
        "{:04}.{:02}.{:02}",
        date / 10_000,
        (date / 100) % 100,
        date % 100
    )
}

fn short_date(date: u32) -> String {
    if date == 0 {
        "—".to_string()
    } else {
        format!("{:04}.{:02}", date / 10_000, (date / 100) % 100)
    }
}

fn trend_label(trend: Option<f64>) -> (String, Color32) {
    match trend {
        Some(delta) if delta > 1.0 => (
            format!("↑ +{delta:.0}pp"),
            Color32::from_rgb(0x8F, 0xC9, 0x97),
        ),
        Some(delta) if delta < -1.0 => (
            format!("↓ {delta:.0}pp"),
            Color32::from_rgb(0xD9, 0x94, 0x94),
        ),
        _ => ("→ 0pp".to_string(), Color32::from_gray(150)),
    }
}

/// Horizontal probability bar used by the player-prediction table. The filled
/// width is the posterior mean; the gold segment marks the 90% interval.
fn probability_bar(ui: &mut Ui, probability: f64, low: f64, high: f64) {
    let (rect, response) = ui.allocate_exact_size(vec2(84.0, 16.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_gray(45));
    let filled = rect.width() * probability.clamp(0.0, 1.0) as f32;
    if filled > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(filled, rect.height())),
            3.0,
            Color32::from_rgb(0x4F, 0x8A, 0x5B),
        );
    }
    let y = rect.center().y + 5.0;
    let low_x = rect.left() + rect.width() * low.clamp(0.0, 1.0) as f32;
    let high_x = rect.left() + rect.width() * high.clamp(0.0, 1.0) as f32;
    painter.line_segment([pos2(low_x, y), pos2(high_x, y)], Stroke::new(2.0, GOLD));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{:.0}%", probability * 100.0),
        FontId::proportional(10.0),
        Color32::WHITE,
    );
    response.on_hover_text(format!(
        "Posterior mean {:.1}% · approximate 90% credible interval {:.1}–{:.1}%",
        probability * 100.0,
        low * 100.0,
        high * 100.0
    ));
}

/// Win/draw/loss bar from the player's perspective.
fn record_bar(ui: &mut Ui, wins: u32, draws: u32, losses: u32) {
    let width = ui.available_width().max(80.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let total = (wins + draws + losses).max(1) as f32;
    let mut x = rect.left();
    for (count, color) in [
        (wins, Color32::from_rgb(0x6F, 0xAE, 0x7A)),
        (draws, Color32::from_rgb(0x87, 0x8C, 0x91)),
        (losses, Color32::from_rgb(0xB5, 0x6B, 0x6B)),
    ] {
        let segment = rect.width() * count as f32 / total;
        if segment > 0.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(segment, rect.height())),
                0.0,
                color,
            );
            x += segment;
        }
    }
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, Color32::from_gray(70)),
        StrokeKind::Inside,
    );
    response.on_hover_text(format!("Wins {wins} · draws {draws} · losses {losses}"));
}

fn wdl_bar(ui: &mut Ui, stats: &PositionStats) {
    let width = ui.available_width().max(80.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let total = stats.games.max(1) as f32;
    let mut x = rect.left();
    let segments = [
        (stats.white, Color32::from_rgb(0xCB, 0xDD, 0xC4)),
        (stats.draw, Color32::from_rgb(0x87, 0x8C, 0x91)),
        (stats.black, Color32::from_rgb(0x2A, 0x2D, 0x30)),
    ];
    for (count, color) in segments {
        let segment = rect.width() * count as f32 / total;
        if segment > 0.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(segment, rect.height())),
                0.0,
                color,
            );
            x += segment;
        }
    }
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(70)),
        egui::StrokeKind::Inside,
    );
    response.on_hover_text(format!(
        "White {} · draw {} · Black {}",
        stats.white, stats.draw, stats.black
    ));
}

/// Normal opening-tree summary and table for one position. Kept free of
/// scrolling so preparation mode can place it inside its own scroll area.
fn opening_tree_table(
    ui: &mut Ui,
    index: Option<&PositionIndex>,
    key: u64,
    pos: &Chess,
    empty_message: &str,
) -> Option<Move> {
    let Some(index) = index else {
        ui.label(RichText::new(empty_message).weak());
        return None;
    };
    let stats = index.stats(key);
    let moves = index.moves(key);

    if let Some(stats) = stats {
        ui.horizontal(|ui| {
            ui.label(RichText::new(games_label(stats.games as usize)).strong());
            ui.label(
                RichText::new(format!(
                    "W {} · D {} · B {}",
                    stats.white, stats.draw, stats.black
                ))
                .weak(),
            );
        });
        wdl_bar(ui, &stats);
        let mut summary = Vec::new();
        if let Some(elo) = stats.elo_avg() {
            summary.push(format!("avg Elo {elo:.0}"));
        }
        let dates = date_range(stats.first_date, stats.last_date);
        if dates != "—" {
            summary.push(dates);
        }
        if !summary.is_empty() {
            ui.label(RichText::new(summary.join(" · ")).weak().small());
        }
    } else {
        ui.label(RichText::new(empty_message).weak());
    }

    if moves.is_empty() {
        return None;
    }

    let mut rows: Vec<(String, Move, position::MoveStat)> = moves
        .into_iter()
        .filter_map(|stat| {
            let mv = stat.to_move(pos)?;
            let san = stat.san(pos)?;
            Some((san, mv, stat))
        })
        .collect();
    rows.sort_by(|a, b| b.2.games.cmp(&a.2.games).then(a.0.cmp(&b.0)));

    let mut clicked = None;
    ui.add_space(4.0);
    egui::Grid::new("opening-tree-grid")
        .num_columns(6)
        .striped(true)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Move").strong());
            ui.label(RichText::new("Games").strong());
            ui.label(RichText::new("Score").strong());
            ui.label(RichText::new("W/D/B").strong());
            ui.label(RichText::new("Elo").strong());
            ui.label(RichText::new("Years").strong());
            ui.end_row();

            for (san, mv, stat) in rows {
                if ui
                    .selectable_label(false, RichText::new(&san).monospace())
                    .on_hover_text("Play this move")
                    .clicked()
                {
                    clicked = Some(mv);
                }
                ui.label(stat.games.to_string());
                ui.label(format!("{:.0}%", stat.score_white() * 100.0));
                ui.label(format!("{}/{}/{}", stat.white, stat.draw, stat.black));
                ui.label(
                    stat.elo_avg()
                        .map(|elo| format!("{elo:.0}"))
                        .unwrap_or_else(|| "—".to_string()),
                );
                ui.label(date_range(stat.first_date, stat.last_date));
                ui.end_row();
            }
        });
    clicked
}

/// Native executable picker for an arbitrary UCI engine.
fn open_engine_dialog() -> Option<std::path::PathBuf> {
    use std::process::Command;

    let dialogs: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Select UCI engine",
                "--file-filter=All files | *",
            ],
        ),
        ("kdialog", &["--getopenfilename", ".", "*|All files"]),
    ];

    for (program, args) in dialogs {
        match Command::new(program).args(args).output() {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!path.is_empty()).then(|| std::path::PathBuf::from(path));
            }
            Ok(_) => return None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }

    None
}

/// Native file dialog. Prefers zenity/kdialog (both common on Linux desktops)
/// and returns `None` when no dialog is available or the user cancels.
fn open_dialog() -> Option<std::path::PathBuf> {
    use std::process::Command;

    let dialogs: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Open chess database",
                "--file-filter=Chess databases | *.pgn *.PGN *.si4 *.si3 *.cbh *.CBH",
                "--file-filter=PGN files | *.pgn *.PGN",
                "--file-filter=SCID databases | *.si4 *.si3",
                "--file-filter=ChessBase databases | *.cbh *.CBH",
                "--file-filter=All files | *",
            ],
        ),
        (
            "kdialog",
            &[
                "--getopenfilename",
                ".",
                "*.pgn *.PGN *.si4 *.si3 *.cbh *.CBH|Chess databases",
            ],
        ),
    ];

    for (program, args) in dialogs {
        match Command::new(program).args(args).output() {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!path.is_empty()).then(|| std::path::PathBuf::from(path));
            }
            // The dialog ran but the user cancelled it.
            Ok(_) => return None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut games = domain::parse_pgn(include_bytes!("../fixtures/opera.pgn")).unwrap();
        let ctx = egui::Context::default();
        App {
            library: None,
            current: Some(games.remove(0)),
            edited: std::collections::HashMap::new(),
            dirty: false,
            game_idx: 0,
            node: 0,
            orientation: Color::White,
            show_clocks: false,
            selected: None,
            targets: Vec::new(),
            drag_from: None,
            status: String::new(),
            index: None,
            index_rx: None,
            index_job: None,
            index_status: String::new(),
            left_tab: LeftTab::Games,
            right_tab: RightTab::Moves,
            players: PlayerDirectory::default(),
            player_filter: String::new(),
            selected_player: None,
            player_book: None,
            player_book_rx: None,
            player_book_job: None,
            player_book_status: String::new(),
            prep: None,
            prep_recent_limit: 10,
            recent_preview: None,
            reference_path: String::new(),
            reference_enabled: false,
            reference_band: 200,
            reference_offset: 0,
            reference: None,
            reference_rx: None,
            reference_job: None,
            reference_status: String::new(),
            annotation_editor: None,
            annotation_focus: false,
            online_open: false,
            online_source: OnlineSource::Lichess,
            online_username: String::new(),
            online_max: 500,
            online_status: String::new(),
            online_rx: None,
            cloud: None,
            cloud_cache: BTreeMap::new(),
            cloud_rx: None,
            cloud_job: None,
            cloud_pending: None,
            cloud_status: String::new(),
            lichess_token: None,
            engine: None,
            engine_status: "Engine not started.".to_string(),
            engine_name: String::new(),
            engine_ready: false,
            engine_auto: true,
            engine_multipv: 3,
            engine_depth: 18,
            engine_time_ms: 1_500,
            engine_use_time: false,
            engine_request: 0,
            engine_fen: None,
            engine_infos: BTreeMap::new(),
            engine_bestmove: None,
            engine_searching: false,
            engine_path: "/usr/bin/stockfish".to_string(),
            engine_threads: 1,
            engine_hash_mb: 16,
            review_engine: None,
            review: None,
            review_status: "No game review yet.".to_string(),
            review_depth: 14,
            review_time_ms: 500,
            review_use_time: true,
            path_input: String::new(),
            filter: String::new(),
            show_about: false,
            textures: PieceTextures::load(&ctx),
            ctx,
        }
    }

    #[test]
    fn annotation_toggles_replace_conflicting_symbols() {
        let mut app = test_app();
        let node = 1; // 1. e4
        app.set_nag(node, Some(5));
        assert_eq!(app.current.as_ref().unwrap().nodes[node].nags, vec![5]);

        app.set_nag(node, Some(13));
        assert_eq!(app.current.as_ref().unwrap().nodes[node].nags, vec![5, 13]);

        app.set_nag(node, Some(14));
        assert_eq!(app.current.as_ref().unwrap().nodes[node].nags, vec![5, 14]);

        app.set_nag(node, Some(5));
        assert_eq!(app.current.as_ref().unwrap().nodes[node].nags, vec![14]);

        app.set_nag(node, None);
        assert!(app.current.as_ref().unwrap().nodes[node].nags.is_empty());
    }

    #[test]
    fn annotation_editor_closes_when_navigating_elsewhere() {
        let mut app = test_app();
        app.annotation_editor = Some(1);
        app.goto(1);
        assert_eq!(app.annotation_editor, Some(1));
        app.goto(2);
        assert_eq!(app.annotation_editor, None);
    }

    #[test]
    fn clicking_the_selected_piece_again_clears_the_hints() {
        let mut app = test_app();
        app.click_square(Square::E2);
        assert_eq!(app.selected, Some(Square::E2));
        assert!(app.targets.contains(&Square::E4));

        app.click_square(Square::E2);
        assert_eq!(app.selected, None);
        assert!(app.targets.is_empty());
    }

    #[test]
    fn drag_start_selects_the_piece_and_drop_plays_the_move() {
        let mut app = test_app();
        app.handle_board_event(BoardEvent::DragStart(Square::E2));
        assert_eq!(app.selected, Some(Square::E2));
        assert_eq!(app.drag_from, Some(Square::E2));
        assert!(app.targets.contains(&Square::E4));

        app.handle_board_event(BoardEvent::Drop {
            from: Square::E2,
            to: Square::E4,
        });
        assert_eq!(app.drag_from, None);
        assert_eq!(app.selected, None);
        assert_eq!(
            app.current.as_ref().unwrap().nodes[app.node].san(),
            Some("e4")
        );
    }

    #[test]
    fn reference_prior_is_used_only_when_enabled() {
        let dir = std::env::temp_dir().join(format!(
            "brett-reference-prior-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pgn_path = dir.join("reference.pgn");
        let index_path = dir.join("reference.brettpos");
        std::fs::write(&pgn_path, include_bytes!("../fixtures/opera.pgn")).unwrap();
        PositionIndex::build_to(&pgn_path, &index_path, BuildOptions::default(), |_| {}).unwrap();
        let index = PositionIndex::open(&index_path).unwrap();
        let mut app = test_app();
        app.reference_path = pgn_path.display().to_string();
        app.reference = Some(ReferenceIndex {
            index,
            job: ReferenceJob {
                path: pgn_path.clone(),
                target: 2_500,
                band: 200,
                offset: 0,
            },
        });
        let key = position::position_key(&shakmaty::Chess::default());

        app.reference_enabled = false;
        let (moves, source) = app.prior_for_position(key);
        assert!(moves.is_empty());
        assert_eq!(source, players::PriorSource::CurrentDatabase);

        app.reference_enabled = true;
        let (moves, source) = app.prior_for_position(key);
        assert!(!moves.is_empty());
        assert_eq!(source, players::PriorSource::Reference);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prep_line_lists_opponent_moves_with_likelihoods() {
        let pgn = b"[Event \"Prep\"]\n[White \"You\"]\n[Black \"Opponent\"]\n[Date \"2024.01.01\"]\n[Result \"*\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n";
        let mut games = domain::parse_pgn(&pgn[..]).unwrap();
        let game = games.remove(0);
        let book = players::PlayerBook::from_games(
            "opponent",
            "Opponent",
            std::slice::from_ref(&game),
            players::PredictionConfig::default(),
            players::today(),
        );

        let mut app = test_app();
        app.node = game.nodes.len() - 1;
        app.current = Some(game);
        app.player_book = Some(book);
        app.prep = Some(PrepSession {
            player_key: "opponent".to_string(),
            player_name: "Opponent".to_string(),
            our_color: Color::White,
            previous_game: None,
        });

        let prep = app.prep.clone().unwrap();
        let rows = app.prep_line_rows(&prep);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "1... e5");
        assert_eq!(
            rows[1].label,
            format!("2... {}", domain::figurine_san("Nc6"))
        );
        assert_eq!(rows[0].date, 2024_0101);
        assert!(rows.iter().all(|row| row.likelihood.is_some()));
        assert!(
            rows.iter()
                .all(|row| row.likelihood_low.is_some() && row.likelihood_high.is_some())
        );
        assert!(rows.iter().all(|row| row.tree_score.is_none()));
    }

    #[test]
    fn recent_preview_is_read_only_and_restores_preparation() {
        let pgn = b"[Event \"Preview\"]\n[White \"Preview White\"]\n[Black \"Preview Black\"]\n[Date \"2025.05.05\"]\n[Result \"*\"]\n\n1. d4 d5 *\n";
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-preview-{}-{}.pgn",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, pgn).unwrap();
        let library = PgnLibrary::build(&path).unwrap();

        let mut app = test_app();
        let prep_game = app.current.clone().unwrap();
        app.library = Some(Arc::new(library) as Arc<dyn GameLibrary>);
        app.node = 2;
        app.right_tab = RightTab::Tree;
        app.prep = Some(PrepSession {
            player_key: "opponent".to_string(),
            player_name: "Opponent".to_string(),
            our_color: Color::White,
            previous_game: None,
        });

        app.open_recent_preview(0);
        assert!(app.prep.is_some());
        assert!(app.recent_preview.is_some());
        assert_eq!(app.current.as_ref().unwrap().event(), "Preview");
        assert_eq!(app.node, 0);
        assert_eq!(app.right_tab, RightTab::Moves);

        let before = app.current.as_ref().unwrap().mainline_len();
        app.handle_board_event(BoardEvent::Drop {
            from: Square::E2,
            to: Square::E4,
        });
        assert_eq!(app.node, 0);
        assert_eq!(app.current.as_ref().unwrap().mainline_len(), before);
        app.play_uci("d2d4");
        assert_eq!(app.node, 0);
        assert_eq!(app.current.as_ref().unwrap().mainline_len(), before);

        app.close_recent_preview();
        assert!(app.prep.is_some());
        assert!(app.recent_preview.is_none());
        assert_eq!(app.current.as_ref().unwrap().event(), prep_game.event());
        assert_eq!(app.node, 2);
        assert_eq!(app.right_tab, RightTab::Tree);

        std::fs::remove_file(path).unwrap();
    }
}
