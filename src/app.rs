use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

use crate::chess::{
    CastlingMode, Chess, Color, EnPassantMode, Fen, Move, Position, SanPlus, Square, UciMove,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    pos2,
    text::{LayoutJob, TextFormat},
    vec2,
};

use crate::book::Book;
use crate::cbh::CbhLibrary;
use crate::champions;
use crate::cloud;
use crate::country::{self, Country};
use crate::domain::{self, Clock, Game, Token};
use crate::engine::{EngineEvent, EngineHandle, EngineInfo, EngineOptions, Score, SearchLimit};
use crate::eval::{self, BoardEval};
use crate::fide;
use crate::library::{GameLibrary, PgnLibrary};
use crate::online::{self, ImportResult, OnlineSource};
use crate::pgn;
use crate::players::{self, PlayerBook, PlayerDirectory, PlayerSummary};
use crate::position::{
    self, BuildOptions, BuildProgress, PositionIndex, PositionStats, RatingFilter,
};
use crate::review::{self, Quality, ReviewConfig, ReviewState};
use crate::review_book::{self, BookSource};
use crate::scid::ScidLibrary;
use crate::settings::{
    ChampionLineage, CloudPrefs, DisplayPrefs, EnginePrefs, PhotoSize, PreparationPrefs,
    RatingPriority,
};
use crate::titles;
use crate::twocbh::TwoCbhLibrary;
use crate::ui::board::{self, BoardEvent, BoardState};
use crate::ui::eval_bar;
use crate::ui::fonts;
use crate::ui::pieces::PieceTextures;

const SURFACE: Color32 = Color32::from_rgb(0x22, 0x25, 0x28);
const VARIATION_BG: Color32 = Color32::from_rgb(0x1D, 0x20, 0x23);
const BORDER: Color32 = Color32::from_rgb(0x38, 0x3C, 0x40);
const GOLD: Color32 = Color32::from_rgb(0xE8, 0xC4, 0x6A);
const GOLD_SOFT: Color32 = Color32::from_rgb(0x4A, 0x3F, 0x24);
const COMMENT_TEXT: Color32 = Color32::from_rgb(0x9E, 0xB0, 0xA9);
/// Warn before writing a PGN larger than this. Plain-text exports are much
/// larger than the compressed databases they come from.
const LARGE_PGN_BYTES: u64 = 100 * 1024 * 1024;
/// Above this size, whole-database work is moved off the UI thread or skipped.
/// The Mega Database 2025 has 11.1 million games; eagerly building its player
/// directory, session rows and position index exhausted RAM before the first
/// move could be browsed.
const LARGE_DATABASE_GAMES: usize = 500_000;
/// Completed reviews kept for reuse during this session.
const REVIEW_CACHE_LIMIT: usize = 16;
/// A full position index is useful for normal collections but is not yet
/// viable for multi-million-game archives. The streaming/external-sort indexer
/// is tracked in `docs/BACKLOG.md` as part of the M2b work.
const FULL_INDEX_MAX_GAMES: usize = 1_000_000;
/// Auto-building a full index above this size makes the app appear hung for
/// minutes. Larger databases wait for an explicit, warned action.
const AUTO_INDEX_MAX_GAMES: usize = 250_000;
/// Reference/tree indexes are built in the background too, so use a file-size
/// guard before opening a multi-gigabyte archive just to reject it.
const LARGE_INDEX_FILE_BYTES: u64 = 1_000_000_000;

fn index_source_too_large(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.len() > LARGE_INDEX_FILE_BYTES)
        .unwrap_or(false)
}
/// Source collections that Brett must never write into. Exports go to a new
/// path chosen by the user, normally under their home directory.
///
/// Contributors can list extra roots in `BRETT_READ_ONLY_ROOTS` (a
/// platform-separated path list). The conventional `$HOME/Chess Databases`
/// and `$HOME/Downloads/CBH` locations are protected automatically when they
/// exist, without baking a particular user's home directory into the source.
fn read_only_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(value) = std::env::var_os("BRETT_READ_ONLY_ROOTS") {
        roots.extend(std::env::split_paths(&value).filter(|path| !path.as_os_str().is_empty()));
    }
    for relative in ["Chess Databases", "Downloads/CBH"] {
        if let Some(path) = home_path(relative).filter(|path| path.exists()) {
            roots.push(path);
        }
    }
    roots
}

struct PlayerBadge {
    name: String,
    title: Option<String>,
    country: Option<&'static str>,
    fide_id: Option<String>,
    rating: Option<String>,
    team: Option<String>,
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

/// A temporary position shown while the pointer rests on a candidate move.
/// It never changes the game tree, the selected node, or the move list.
#[derive(Clone, Debug)]
struct HoverPreview {
    pos: Chess,
    last_move: Option<(Square, Square)>,
}

/// One move of a principal variation with the position after it.
#[derive(Clone, Debug)]
struct PvStep {
    san: String,
    number: u32,
    color: Color,
    preview: HoverPreview,
}

/// A keyboard-driven choice between the continuations of one position.
#[derive(Clone, Debug)]
struct BranchChoice {
    parent: usize,
    /// Mainline child first, then variations by most recent visit.
    options: Vec<usize>,
    selected: usize,
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
    References,
}

impl RightTab {
    const ALL: [RightTab; 4] = [
        RightTab::Moves,
        RightTab::Tree,
        RightTab::Engine,
        RightTab::References,
    ];

    fn index(self) -> usize {
        match self {
            Self::Moves => 0,
            Self::Tree => 1,
            Self::Engine => 2,
            Self::References => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Moves => "Moves",
            Self::Tree => "Tree",
            Self::Engine => "Engine",
            Self::References => "Refs",
        }
    }
}

enum IndexMessage {
    Progress(BuildProgress),
    Done(Result<PositionIndex, String>),
}

enum OpenMessage {
    Progress(String),
    Done(Result<Arc<dyn GameLibrary>, String>),
}

enum TreeIndexMessage {
    Progress(BuildProgress),
    Done(Result<(Arc<dyn GameLibrary>, PositionIndex), String>),
}

enum CorrespondenceMessage {
    Progress(BuildProgress),
    Done(Result<PositionIndex, String>),
}

enum BookMessage {
    Done(Result<Arc<Book>, String>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EngineView {
    Local,
    Cloud,
    #[default]
    Both,
}

impl EngineView {
    fn config_value(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::Both => "both",
        }
    }

    fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Self::Local,
            "cloud" => Self::Cloud,
            _ => Self::Both,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineLimitMode {
    #[default]
    Infinite,
    Depth,
    Time,
}

impl EngineLimitMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Infinite => "Infinite",
            Self::Depth => "Depth",
            Self::Time => "Time",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "depth" => Self::Depth,
            "time" => Self::Time,
            _ => Self::Infinite,
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Infinite => "infinite",
            Self::Depth => "depth",
            Self::Time => "time",
        }
    }
}

pub struct EngineSlotState {
    pub enabled: bool,
    pub path: String,
    pub threads: u32,
    pub hash_mb: u32,
    pub engine: Option<EngineHandle>,
    pub name: String,
    pub ready: bool,
    pub searching: bool,
    pub request: u64,
    pub fen: Option<String>,
    pub infos: BTreeMap<usize, EngineInfo>,
    pub bestmove: Option<String>,
    pub status: String,
    pub debouncer: crate::analysis_cache::AnalysisDebouncer,
}

impl EngineSlotState {
    pub fn new(enabled: bool, path: String, threads: u32, hash_mb: u32) -> Self {
        Self {
            enabled,
            path,
            threads,
            hash_mb,
            engine: None,
            name: String::new(),
            ready: false,
            searching: false,
            request: 0,
            fen: None,
            infos: BTreeMap::new(),
            bestmove: None,
            status: "Engine not started.".to_string(),
            debouncer: crate::analysis_cache::AnalysisDebouncer::new(),
        }
    }

    pub fn short_name(&self, idx: usize) -> String {
        if !self.name.is_empty() {
            self.name
                .split_whitespace()
                .next()
                .unwrap_or("Engine")
                .to_string()
        } else if !self.path.trim().is_empty() {
            Path::new(&self.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Engine")
                .to_string()
        } else if idx == 0 {
            "Stockfish".to_string()
        } else {
            format!("Engine {}", idx + 1)
        }
    }
}

enum PlayerBookMessage {
    Progress { done: usize, total: usize },
    Done(Result<PlayerBook, String>),
}

enum PlayerDirectoryMessage {
    Done(PlayerDirectory),
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

enum ExplorerMessage {
    Done {
        fen: String,
        source: cloud::ExplorerSource,
        result: Result<cloud::Explorer, String>,
    },
}

struct BarCloudMessage {
    fen: String,
    result: Result<Option<cloud::CloudEval>, String>,
}

struct ReviewBookMessage {
    game: Game,
    config: ReviewConfig,
    book_plies: usize,
}

enum WikiMessage {
    Done {
        player_key: String,
        result: Result<crate::wiki::Lookup, String>,
    },
}

/// State of the Wikipedia card for the profile currently on screen.
#[derive(Clone, Debug)]
enum WikiState {
    Idle,
    Loading,
    Ready(crate::wiki::Lookup),
    Failed(String),
}

#[derive(Clone, Debug)]
struct PrepSession {
    player_key: String,
    player_name: String,
    player_title: Option<titles::PlayerTitle>,
    player_fide_id: Option<String>,
    our_color: Color,
    previous_game: Option<usize>,
    previous_orientation: Color,
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
    white: String,
    black: String,
    white_title: Option<&'static str>,
    black_title: Option<&'static str>,
    white_country: Option<&'static str>,
    black_country: Option<&'static str>,
    white_fide_id: Option<String>,
    black_fide_id: Option<String>,
    subtitle: String,
}

/// Read-only game preview. The previous game, row, and cursor are kept intact
/// so closing the preview can restore them exactly.
#[derive(Clone, Debug)]
struct RecentPreview {
    return_game: Option<Game>,
    return_slot: Option<usize>,
    return_node: usize,
    return_tab: RightTab,
    return_orientation: Color,
    return_print_diagrams: BTreeSet<usize>,
    label: String,
}

#[derive(Clone, Debug)]
struct PendingPgnExport {
    path: PathBuf,
    games: usize,
    estimate: u64,
    close_after: bool,
}

#[derive(Clone, Debug)]
struct PendingScidExport {
    path: PathBuf,
    games: usize,
    existing: Vec<PathBuf>,
    close_after: bool,
}

#[derive(Clone, Debug)]
struct PendingCbhExport {
    path: PathBuf,
    games: usize,
    existing: Vec<PathBuf>,
    close_after: bool,
}

#[derive(Clone, Debug)]
struct PendingTwoCbhExport {
    path: PathBuf,
    games: usize,
    existing: Vec<PathBuf>,
    annotations: bool,
    close_after: bool,
}

/// A database container Brett can open and write. 2CBH writing covers moves,
/// variations, and metadata; annotations are not written yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseFormat {
    Pgn,
    Scid,
    Cbh,
    TwoCbh,
}

impl DatabaseFormat {
    /// Formats offered when the user has to choose a save format.
    const WRITABLE: [Self; 4] = [Self::Pgn, Self::Scid, Self::Cbh, Self::TwoCbh];

    fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "si4" | "si3" => Self::Scid,
            "cbh" | "cbv" => Self::Cbh,
            "2cbh" | "2cbg" | "2cba" | "2lid" => Self::TwoCbh,
            _ => Self::Pgn,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pgn => "PGN (plain text, portable)",
            Self::Scid => "SCID4 database (.si4)",
            Self::Cbh => "Classic ChessBase database (.cbh)",
            Self::TwoCbh => "ChessBase 2CBH database (.2cbh)",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Pgn => "pgn",
            Self::Scid => "si4",
            Self::Cbh => "cbh",
            Self::TwoCbh => "2cbh",
        }
    }

    fn filter_name(self) -> &'static str {
        match self {
            Self::Pgn => "PGN files",
            Self::Scid => "SCID4 database",
            Self::Cbh => "Classic ChessBase database",
            Self::TwoCbh => "ChessBase 2CBH database",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Pgn => "PGN",
            Self::Scid => "SCID",
            Self::Cbh => "CBH",
            Self::TwoCbh => "2CBH",
        }
    }

    fn is_writable(self) -> bool {
        Self::WRITABLE.contains(&self)
    }
}

/// Default save path for a database in `format`. When the format matches the
/// open source, append `-saved` so Save never overwrites the read-only source.
fn database_save_default_path(
    stem: &str,
    source: Option<&Path>,
    directory: &Path,
    format: DatabaseFormat,
) -> PathBuf {
    let stem = stem.trim();
    let stem = if stem.is_empty() {
        "Brett Database"
    } else {
        stem
    };
    let same_format = source.is_some_and(|path| DatabaseFormat::from_path(path) == format);
    let stem = if same_format {
        format!("{stem}-saved")
    } else {
        stem.to_string()
    };
    directory.join(format!("{stem}.{}", format.extension()))
}

/// Small dependency-free calendar used by the Date PGN tag.
#[derive(Clone, Debug)]
struct DatePicker {
    year: i32,
    month: u32,
    manual: String,
}

/// Another database shown in the floating Database views window.
struct DatabaseView {
    path: PathBuf,
    library: Arc<dyn GameLibrary>,
    session: crate::session::GameList,
    filter: String,
}

impl DatabaseView {
    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Clone, Copy, Debug)]
enum DropTarget {
    Main,
    View(usize),
}

struct PendingGameDrop {
    game: Game,
    target: DropTarget,
    insert_at: usize,
    label: String,
}

struct PendingMerge {
    library: Arc<dyn GameLibrary>,
    target: DropTarget,
    label: String,
}

struct PendingOpenChoice {
    path: PathBuf,
}

#[derive(Clone, Default)]
pub(crate) struct CloudEngineRow {
    uci: String,
    san: String,
    chessdb: Option<i32>,
    lichess: Option<(Option<i32>, Option<i32>)>,
    lichess_depth: Option<u32>,
    stockfish: Option<(Option<i32>, Option<i32>)>,
    stockfish_depth: Option<u32>,
    stockfish_name: Option<String>,
    leela: Option<(Option<i32>, Option<i32>)>,
    leela_depth: Option<u32>,
    leela_name: Option<String>,
    variation: Vec<String>,
    variation_source: Option<String>,
    alt_variation: Option<Vec<String>>,
    alt_variation_source: Option<String>,
}

pub struct App {
    library: Option<Arc<dyn GameLibrary>>,
    /// Ordered, editable view of the games currently shown in the Games tab.
    session: crate::session::GameList,
    current: Option<Game>,
    /// Games edited during this session, keyed by stable row id so switching
    /// away and back does not discard new moves or variations.
    edited: std::collections::HashMap<usize, Game>,
    dirty: bool,
    views: Vec<DatabaseView>,
    views_open: bool,
    pending_game_drop: Option<PendingGameDrop>,
    pending_merge: Option<PendingMerge>,
    pending_open_choice: Option<PendingOpenChoice>,
    current_slot: Option<usize>,
    node: usize,
    orientation: Color,
    show_clocks: bool,
    selected: Option<Square>,
    targets: Vec<Square>,
    drag_from: Option<Square>,
    status: String,
    open_rx: Option<mpsc::Receiver<OpenMessage>>,
    open_job: Option<PathBuf>,
    index: Option<PositionIndex>,
    index_rx: Option<mpsc::Receiver<IndexMessage>>,
    index_job: Option<PathBuf>,
    index_status: String,
    confirm_large_index: bool,
    /// Folder of the last database opened or saved. Database pickers start
    /// here so the user usually lands in the right directory.
    last_database_dir: Option<PathBuf>,
    /// Folder of the last print export. Print exports start here so
    /// repeated exports from the same session stay together.
    last_print_dir: Option<PathBuf>,
    /// Last writable database path chosen in this session. Save database
    /// writes here without asking again.
    database_save_path: Option<PathBuf>,
    /// The game list changed independently of the current game's edits.
    session_dirty: bool,
    /// The window close request is waiting for Save / Discard / Cancel.
    close_confirm_open: bool,
    /// Close the window on the next frame, after a successful save or a
    /// confirmed discard.
    pending_window_close: bool,
    /// The in-app format/path chooser for Save database or Save database as.
    save_database_dialog_open: bool,
    save_database_as: bool,
    save_database_format: DatabaseFormat,
    save_database_path: String,
    save_database_close_after: bool,
    save_database_error: String,
    /// Optional default opening-tree database, indexed separately from the
    /// currently open database.
    tree_path: String,
    tree_index: Option<PositionIndex>,
    tree_index_rx: Option<mpsc::Receiver<TreeIndexMessage>>,
    tree_index_job: Option<PathBuf>,
    tree_library: Option<Arc<dyn GameLibrary>>,
    tree_status: String,
    /// Optional native opening book shown in the Tree tab.
    book_path: String,
    book: Option<Arc<Book>>,
    book_rx: Option<mpsc::Receiver<BookMessage>>,
    book_job: Option<PathBuf>,
    book_status: String,
    left_tab: LeftTab,
    right_tab: RightTab,
    /// Show several right-panel sections at once.
    multi_view: bool,
    visible_tabs: [bool; 4],
    players: PlayerDirectory,
    players_rx: Option<mpsc::Receiver<PlayerDirectoryMessage>>,
    players_job: Option<PathBuf>,
    players_status: String,
    player_filter: String,
    selected_player: Option<usize>,
    /// Wikipedia summaries are fetched only for the opened profile.
    wiki_enabled: bool,
    wiki_cache: crate::wiki::Cache,
    wiki_rx: Option<mpsc::Receiver<WikiMessage>>,
    wiki_job: Option<String>,
    wiki_for: Option<String>,
    wiki_state: WikiState,
    wiki_expanded: bool,
    player_book: Option<PlayerBook>,
    player_book_rx: Option<mpsc::Receiver<PlayerBookMessage>>,
    player_book_job: Option<String>,
    player_book_status: String,
    prep: Option<PrepSession>,
    prep_recent_limit: usize,
    /// Ordered aliases for the user. The first match sets board orientation on
    /// open; the first alias replaces `You` in preparation tags.
    prep_player_names: Vec<String>,
    recent_preview: Option<RecentPreview>,
    reference_path: String,
    reference_enabled: bool,
    reference_band: u32,
    reference_offset: i32,
    reference: Option<ReferenceIndex>,
    reference_rx: Option<mpsc::Receiver<ReferenceMessage>>,
    reference_job: Option<ReferenceJob>,
    reference_status: String,
    /// Optional local correspondence database used automatically in the
    /// Engine tab as a third position reference.
    correspondence_enabled: bool,
    correspondence_path: String,
    correspondence: Option<PositionIndex>,
    correspondence_rx: Option<mpsc::Receiver<CorrespondenceMessage>>,
    correspondence_job: Option<PathBuf>,
    correspondence_status: String,
    /// Move whose annotation editor is open. Clicking an annotation or using
    /// the move context menu enters this mode.
    annotation_editor: Option<usize>,
    /// Request keyboard focus on the annotation comment box once.
    annotation_focus: bool,
    /// Whether the additional (less common) PGN tags are expanded.
    tags_expanded: bool,
    /// Show PGN fields that are absent or empty.
    show_empty_pgn_tags: bool,
    /// Board was added manually to the current game even though its Round
    /// value has no board part yet.
    board_tag_visible: bool,
    /// Tags added manually during this game, kept visible even while empty.
    forced_tag_visibility: HashSet<String>,
    /// Open branch-selection popup, if any.
    branch_choice: Option<BranchChoice>,
    /// Session-local recency of visited variations, keyed by node id.
    branch_recency: std::collections::HashMap<usize, u64>,
    branch_clock: u64,
    /// Preview moves on the board when hovering the open game's move list.
    hover_moves: bool,
    /// Optional player-photo pack, memory-mapped at startup.
    photos: Option<crate::photos::PhotoLibrary>,
    /// Decoded portraits, kept small so browsing many games cannot exhaust GPU
    /// memory.
    photo_textures: std::collections::HashMap<String, egui::TextureHandle>,
    photo_order: VecDeque<String>,
    show_player_photos: bool,
    /// Avatar size for board strips and the selected-player header.
    photo_size: PhotoSize,
    /// Championship lineage used for the gold crown.
    champion_lineage: ChampionLineage,
    /// Player whose portrait is open in the standalone viewer.
    photo_viewer: Option<String>,
    show_nav_buttons: bool,
    show_pgn_tags: bool,
    show_annotations: bool,
    engine_bar: bool,
    bar_cloud_cache: BTreeMap<String, Option<cloud::CloudEval>>,
    bar_cloud_rx: Option<mpsc::Receiver<BarCloudMessage>>,
    bar_cloud_job: Option<String>,
    bar_cloud_pending: Option<(String, Instant)>,
    /// Open calendar for the Date tag.
    date_picker: Option<DatePicker>,
    /// Screen position of the date field that opened the calendar.
    date_picker_anchor: Option<egui::Pos2>,
    /// Preferred rating source when several rating tags are present.
    rating_priority: RatingPriority,
    /// Include every available rating instead of only the preferred source.
    show_all_ratings: bool,
    /// Position temporarily overlaid on the board while a candidate move is
    /// hovered in the tree, engine, or reference panels.
    hover_preview: Option<HoverPreview>,
    online_open: bool,
    online_source: OnlineSource,
    online_username: String,
    online_max: usize,
    online_status: String,
    online_rx: Option<mpsc::Receiver<OnlineMessage>>,
    online_game_rx: Option<mpsc::Receiver<Result<Game, String>>>,
    online_game_job: Option<String>,
    online_reference_open: bool,
    online_ref_source: OnlineSource,
    online_ref_username: String,
    online_ref_games: Vec<Game>,
    online_ref_status: String,
    online_ref_rx: Option<mpsc::Receiver<Result<Vec<Game>, String>>>,
    online_ref_job: Option<(OnlineSource, String)>,
    online_ref_at_position: bool,
    /// Latest ChessABC games for the player currently being prepared.
    prep_online_games: Vec<Game>,
    prep_online_status: String,
    prep_online_rx: Option<mpsc::Receiver<Result<Vec<Game>, String>>>,
    prep_online_job: Option<String>,
    cloud: Option<cloud::Snapshot>,
    cloud_cache: BTreeMap<(cloud::ExplorerSource, String), cloud::Snapshot>,
    cloud_rx: Option<mpsc::Receiver<CloudMessage>>,
    cloud_job: Option<String>,
    explorer_rx: Option<mpsc::Receiver<ExplorerMessage>>,
    explorer_job: Option<(String, cloud::ExplorerSource)>,
    cloud_pending: Option<(String, Instant)>,
    cloud_status: String,
    cached_analyses: Vec<crate::analysis_cache::CachedAnalysis>,
    lichess_token: Option<String>,
    explorer_source: cloud::ExplorerSource,
    engine_slots: [EngineSlotState; 3],
    selected_engine_slot: usize,
    engine_view: EngineView,
    engine_status: String,
    engine_name: String,
    engine_ready: bool,
    engine_auto: bool,
    engine_multipv: usize,
    engine_depth: u32,
    engine_time_ms: u64,
    engine_limit_mode: EngineLimitMode,
    engine_cache_saved_notice: Option<(u64, u32, Instant)>,
    cached_analyses_pos: Option<u64>,
    engine_request: u64,
    engine_fen: Option<String>,
    engine_infos: BTreeMap<usize, EngineInfo>,
    engine_bestmove: Option<String>,
    engine_searching: bool,
    engine_path: String,
    engine_threads: u32,
    engine_hash_mb: u32,
    cache_engine_analysis: bool,
    heuristic_supersede: bool,
    analysis_cache: Option<crate::analysis_cache::AnalysisCacheHandle>,
    review_engine: Option<EngineHandle>,
    review: Option<ReviewState>,
    review_open: bool,
    engine_settings_open: bool,
    review_status: String,
    review_depth: u32,
    review_time_ms: u64,
    review_use_time: bool,
    review_skip_book: bool,
    review_book_min_games: u32,
    review_book_max_plies: u32,
    review_book_rx: Option<mpsc::Receiver<ReviewBookMessage>>,
    review_cache: Vec<(review::ReviewKey, ReviewState)>,
    trend_window: crate::recent::TrendWindow,
    trend_baseline: crate::recent::TrendBaseline,
    trend_min_games: u32,
    filter: String,
    pending_pgn_export: Option<PendingPgnExport>,
    pending_scid_export: Option<PendingScidExport>,
    pending_cbh_export: Option<PendingCbhExport>,
    pending_two_cbh_export: Option<PendingTwoCbhExport>,
    /// Node ids where the user asked for a printed diagram. The final
    /// position is controlled separately by `print_export_include_final`.
    print_diagrams: BTreeSet<usize>,
    print_export_open: bool,
    print_export_path: String,
    print_export_include_final: bool,
    print_export_compile_pdf: bool,
    print_export_status: String,
    setup_open: bool,
    setup_state: crate::setup::PositionSetupState,
    classify_openings_open: bool,
    classify_overwrite: bool,
    classify_status: String,
    show_about: bool,
    show_cache_info: bool,
    show_settings: bool,
    settings_rating_priority: RatingPriority,
    settings_show_all_ratings: bool,
    settings_photo_size: PhotoSize,
    settings_champion_lineage: ChampionLineage,
    settings_show_clocks: bool,
    settings_show_annotations: bool,
    settings_show_pgn_tags: bool,
    settings_show_nav_buttons: bool,
    settings_show_player_photos: bool,
    settings_show_empty_pgn_tags: bool,
    settings_online_reference: bool,
    settings_engine_bar: bool,
    settings_my_player_names: String,
    settings_correspondence_enabled: bool,
    settings_correspondence_path: String,
    settings_review_skip_book: bool,
    settings_review_book_min_games: u32,
    settings_review_book_max_plies: u32,
    settings_trend_window: crate::recent::TrendWindow,
    settings_trend_baseline: crate::recent::TrendBaseline,
    settings_trend_min_games: u32,
    settings_token: String,
    settings_token_visible: bool,
    settings_token_from_env: bool,
    settings_wiki_enabled: bool,
    settings_status: String,
    textures: PieceTextures,
    ctx: egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut args = std::env::args_os().skip(1).map(std::path::PathBuf::from);
        let start = args.next();
        let views = args.collect();
        Self::with_file(cc, start, views)
    }

    pub fn with_file(
        cc: &eframe::CreationContext<'_>,
        start: Option<std::path::PathBuf>,
        views: Vec<std::path::PathBuf>,
    ) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(0x1B, 0x1D, 0x1F);
        visuals.window_fill = Color32::from_rgb(0x22, 0x24, 0x27);
        visuals.extreme_bg_color = Color32::from_rgb(0x14, 0x15, 0x17);
        cc.egui_ctx.set_visuals(visuals);
        fonts::install(&cc.egui_ctx);
        let engine_prefs = EnginePrefs::load();
        let preparation_prefs = PreparationPrefs::load();
        let cloud_prefs = CloudPrefs::load();
        let display_prefs = DisplayPrefs::load();
        let (online_ref_source, online_ref_username) =
            if let Some(username) = cloud_prefs.lichess_username.clone() {
                (OnlineSource::Lichess, username)
            } else if let Some(username) = cloud_prefs.chesscom_username.clone() {
                (OnlineSource::ChessCom, username)
            } else {
                (OnlineSource::Lichess, String::new())
            };
        let reference_path = preparation_prefs
            .reference_path
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| default_reference_path().map(|path| path.display().to_string()))
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
        let correspondence_path = preparation_prefs
            .correspondence_path
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| default_correspondence_path().map(|path| path.display().to_string()))
            .unwrap_or_default();
        let correspondence_status = if !preparation_prefs.correspondence_enabled {
            "Correspondence database disabled in Settings.".to_string()
        } else if correspondence_path.is_empty() {
            "No correspondence database configured.".to_string()
        } else {
            "Correspondence database configured; loading…".to_string()
        };

        let mut app = Self {
            library: None,
            session: crate::session::GameList::empty(),
            current: None,
            edited: std::collections::HashMap::new(),
            dirty: false,
            views: Vec::new(),
            views_open: false,
            pending_game_drop: None,
            pending_merge: None,
            pending_open_choice: None,
            current_slot: None,
            node: 0,
            orientation: Color::White,
            show_clocks: display_prefs.show_clocks,
            selected: None,
            targets: Vec::new(),
            drag_from: None,
            status: "Open a PGN, SCID, or classic ChessBase database, or drop one onto the window."
                .to_string(),
            open_rx: None,
            open_job: None,
            index: None,
            index_rx: None,
            index_job: None,
            index_status: "No position index yet.".to_string(),
            confirm_large_index: false,
            last_database_dir: display_prefs.last_database_dir.clone(),
            last_print_dir: display_prefs.last_print_dir.clone(),
            database_save_path: None,
            session_dirty: false,
            close_confirm_open: false,
            pending_window_close: false,
            save_database_dialog_open: false,
            save_database_as: false,
            save_database_format: DatabaseFormat::Pgn,
            save_database_path: String::new(),
            save_database_close_after: false,
            save_database_error: String::new(),
            tree_path: display_prefs.opening_tree_path.clone(),
            tree_index: None,
            tree_index_rx: None,
            tree_index_job: None,
            tree_library: None,
            tree_status: if display_prefs.opening_tree_path.trim().is_empty() {
                "Using the current database for the opening tree.".to_string()
            } else {
                "Opening-tree file configured.".to_string()
            },
            book_path: display_prefs.opening_book_path.clone(),
            book: None,
            book_rx: None,
            book_job: None,
            book_status: if display_prefs.opening_book_path.trim().is_empty() {
                "No opening book loaded.".to_string()
            } else {
                "Opening book configured.".to_string()
            },
            left_tab: LeftTab::Games,
            right_tab: RightTab::Moves,
            multi_view: false,
            visible_tabs: [true, false, false, false],
            players: PlayerDirectory::default(),
            players_rx: None,
            players_job: None,
            players_status: "No players indexed.".to_string(),
            player_filter: String::new(),
            selected_player: None,
            wiki_enabled: cloud_prefs.wikipedia_summaries,
            wiki_cache: crate::wiki::Cache::load(),
            wiki_rx: None,
            wiki_job: None,
            wiki_for: None,
            wiki_state: WikiState::Idle,
            wiki_expanded: false,
            player_book: None,
            player_book_rx: None,
            player_book_job: None,
            player_book_status: "Select a player to build their preparation book.".to_string(),
            prep: None,
            prep_recent_limit: display_prefs.prep_recent_limit,
            prep_player_names: preparation_prefs.prep_player_names.clone(),
            recent_preview: None,
            reference_path,
            reference_enabled: preparation_prefs.reference_enabled,
            reference_band: preparation_prefs.rating_band,
            reference_offset: preparation_prefs.rating_offset,
            reference: None,
            reference_rx: None,
            reference_job: None,
            reference_status,
            correspondence_enabled: preparation_prefs.correspondence_enabled,
            correspondence_path,
            correspondence: None,
            correspondence_rx: None,
            correspondence_job: None,
            correspondence_status,
            annotation_editor: None,
            annotation_focus: false,
            tags_expanded: false,
            show_empty_pgn_tags: display_prefs.show_empty_pgn_tags,
            board_tag_visible: false,
            forced_tag_visibility: HashSet::new(),
            branch_choice: None,
            branch_recency: std::collections::HashMap::new(),
            branch_clock: 0,
            hover_moves: display_prefs.hover_moves,
            photos: crate::photos::default_path().and_then(|path| {
                match crate::photos::PhotoLibrary::open(&path) {
                    Ok(library) => Some(library),
                    Err(err) => {
                        tracing::warn!("player photos unavailable: {err:#}");
                        None
                    }
                }
            }),
            photo_textures: std::collections::HashMap::new(),
            photo_order: VecDeque::new(),
            show_player_photos: display_prefs.show_player_photos,
            photo_size: display_prefs.photo_size,
            champion_lineage: display_prefs.champion_lineage,
            photo_viewer: None,
            show_nav_buttons: display_prefs.show_nav_buttons,
            show_pgn_tags: display_prefs.show_pgn_tags,
            show_annotations: display_prefs.show_annotations,
            engine_bar: display_prefs.engine_bar,
            bar_cloud_cache: BTreeMap::new(),
            bar_cloud_rx: None,
            bar_cloud_job: None,
            bar_cloud_pending: None,
            date_picker: None,
            date_picker_anchor: None,
            rating_priority: display_prefs.rating_priority,
            show_all_ratings: display_prefs.show_all_ratings,
            hover_preview: None,
            online_open: false,
            online_source: OnlineSource::Lichess,
            online_username: String::new(),
            online_max: 500,
            online_status: String::new(),
            online_rx: None,
            online_game_rx: None,
            online_game_job: None,
            online_reference_open: false,
            online_ref_source,
            online_ref_username,
            online_ref_games: Vec::new(),
            online_ref_status:
                "Load a Lichess or Chess.com account to compare it with the database.".to_string(),
            online_ref_rx: None,
            online_ref_job: None,
            online_ref_at_position: false,
            prep_online_games: Vec::new(),
            prep_online_status: String::new(),
            prep_online_rx: None,
            prep_online_job: None,
            cloud: None,
            cloud_cache: BTreeMap::new(),
            cloud_rx: None,
            cloud_job: None,
            explorer_rx: None,
            explorer_job: None,
            cloud_pending: None,
            cloud_status: "Open a game and select References.".to_string(),
            cached_analyses: Vec::new(),
            lichess_token: cloud_prefs.lichess_token,
            explorer_source: cloud_prefs.explorer_source,
            engine_slots: [
                EngineSlotState::new(
                    engine_prefs.engines[0].enabled,
                    engine_prefs.engines[0]
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| engine_prefs.resolved_path().display().to_string()),
                    engine_prefs.engines[0].threads,
                    engine_prefs.engines[0].hash_mb,
                ),
                EngineSlotState::new(
                    engine_prefs.engines[1].enabled,
                    engine_prefs.engines[1]
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    engine_prefs.engines[1].threads,
                    engine_prefs.engines[1].hash_mb,
                ),
                EngineSlotState::new(
                    engine_prefs.engines[2].enabled,
                    engine_prefs.engines[2]
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    engine_prefs.engines[2].threads,
                    engine_prefs.engines[2].hash_mb,
                ),
            ],
            selected_engine_slot: 0,
            engine_view: EngineView::from_config(&display_prefs.engine_view),
            engine_status: "Engine not started.".to_string(),
            engine_name: String::new(),
            engine_ready: false,
            engine_auto: display_prefs.engine_auto,
            engine_multipv: 3,
            engine_depth: display_prefs.engine_depth,
            engine_time_ms: display_prefs.engine_time_ms,
            engine_limit_mode: EngineLimitMode::from_config(&display_prefs.engine_limit_mode),
            engine_cache_saved_notice: None,
            cached_analyses_pos: None,
            engine_request: 0,
            engine_fen: None,
            engine_infos: BTreeMap::new(),
            engine_bestmove: None,
            engine_searching: false,
            engine_path: engine_prefs.resolved_path().display().to_string(),
            engine_threads: engine_prefs.threads,
            engine_hash_mb: engine_prefs.hash_mb,
            cache_engine_analysis: engine_prefs.cache_engine_analysis,
            heuristic_supersede: engine_prefs.heuristic_supersede,
            analysis_cache: Some(crate::analysis_cache::AnalysisCacheHandle::spawn(
                None,
                engine_prefs.heuristic_supersede,
            )),
            review_engine: None,
            review: None,
            review_open: false,
            engine_settings_open: false,
            review_status: "No game review yet.".to_string(),
            review_depth: 14,
            review_time_ms: 500,
            review_use_time: true,
            review_skip_book: display_prefs.review_skip_book,
            review_book_min_games: display_prefs.review_book_min_games,
            review_book_max_plies: display_prefs.review_book_max_plies,
            review_book_rx: None,
            review_cache: Vec::new(),
            trend_window: display_prefs.trend_window,
            trend_baseline: display_prefs.trend_baseline,
            trend_min_games: display_prefs.trend_min_games,
            filter: String::new(),
            pending_pgn_export: None,
            pending_scid_export: None,
            pending_cbh_export: None,
            pending_two_cbh_export: None,
            print_diagrams: BTreeSet::new(),
            print_export_open: false,
            print_export_path: String::new(),
            print_export_include_final: true,
            print_export_compile_pdf: false,
            print_export_status: String::new(),
            setup_open: false,
            setup_state: crate::setup::PositionSetupState::default(),
            classify_openings_open: false,
            classify_overwrite: false,
            classify_status: String::new(),
            show_about: false,
            show_cache_info: false,
            show_settings: false,
            settings_rating_priority: display_prefs.rating_priority,
            settings_show_all_ratings: display_prefs.show_all_ratings,
            settings_photo_size: display_prefs.photo_size,
            settings_champion_lineage: display_prefs.champion_lineage,
            settings_show_clocks: display_prefs.show_clocks,
            settings_show_annotations: display_prefs.show_annotations,
            settings_show_pgn_tags: display_prefs.show_pgn_tags,
            settings_show_nav_buttons: display_prefs.show_nav_buttons,
            settings_show_player_photos: display_prefs.show_player_photos,
            settings_show_empty_pgn_tags: display_prefs.show_empty_pgn_tags,
            settings_online_reference: false,
            settings_engine_bar: display_prefs.engine_bar,
            settings_my_player_names: String::new(),
            settings_correspondence_enabled: preparation_prefs.correspondence_enabled,
            settings_correspondence_path: String::new(),
            settings_review_skip_book: display_prefs.review_skip_book,
            settings_review_book_min_games: display_prefs.review_book_min_games,
            settings_review_book_max_plies: display_prefs.review_book_max_plies,
            settings_trend_window: display_prefs.trend_window,
            settings_trend_baseline: display_prefs.trend_baseline,
            settings_trend_min_games: display_prefs.trend_min_games,
            settings_token: String::new(),
            settings_token_visible: false,
            settings_token_from_env: false,
            settings_wiki_enabled: cloud_prefs.wikipedia_summaries,
            settings_status: String::new(),
            textures: PieceTextures::load(&cc.egui_ctx),
            ctx: cc.egui_ctx.clone(),
        };

        if let Some(path) = start {
            app.load_file(&path);
        }
        for path in views {
            app.open_database_view(&path);
        }
        if !app.tree_path.trim().is_empty() {
            let path = PathBuf::from(app.tree_path.trim());
            if path.exists() {
                app.start_tree_index(path);
            } else {
                app.tree_status = format!("Opening-tree file not found: {}", path.display());
            }
        }
        if !app.book_path.trim().is_empty() {
            let path = PathBuf::from(app.book_path.trim());
            if path.exists() {
                app.start_book(path);
            } else {
                app.book_status = format!("Opening book not found: {}", path.display());
            }
        }
        if app.correspondence_enabled {
            if !app.correspondence_path.trim().is_empty() {
                let path = PathBuf::from(app.correspondence_path.trim());
                if path.exists() {
                    app.start_correspondence(path);
                } else {
                    app.correspondence_status =
                        format!("Correspondence database not found: {}", path.display());
                }
            }
        } else {
            app.correspondence_status = "Correspondence database disabled in Settings.".to_string();
        }
        app
    }

    fn game(&self) -> Option<&Game> {
        self.current.as_ref()
    }

    fn goto(&mut self, node: usize) {
        self.branch_choice = None;
        if let Some(game) = &self.current
            && let Some(parent) = game.nodes.get(node).and_then(|node| node.parent)
            && game.nodes[parent].children.first().copied() != Some(node)
        {
            self.branch_clock = self.branch_clock.wrapping_add(1);
            self.branch_recency.insert(node, self.branch_clock);
        }
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

    fn open_date_picker(&mut self) {
        let value = self
            .current
            .as_ref()
            .and_then(|game| game.header("Date"))
            .unwrap_or("")
            .to_string();
        let today = players::today();
        let (fallback_year, fallback_month) = ((today / 10_000) as i32, (today / 100) % 100);
        let mut parts = value.split('.');
        let year = parts.next().and_then(|year| year.parse::<i32>().ok());
        let month = parts.next().and_then(|month| month.parse::<u32>().ok());
        let (year, month) = match (year, month) {
            (Some(year), Some(month)) if (1..=12).contains(&month) => (year, month),
            _ => (fallback_year, fallback_month),
        };
        self.date_picker = Some(DatePicker {
            year,
            month,
            manual: value,
        });
    }

    fn date_picker_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.date_picker.take() else {
            return;
        };
        let mut open = true;
        let mut chosen = None;
        let screen = ctx.content_rect();
        let anchor = self
            .date_picker_anchor
            .unwrap_or_else(|| egui::pos2(screen.center().x - 150.0, screen.center().y - 170.0));
        let position = egui::pos2(
            anchor.x.clamp(
                screen.left() + 8.0,
                (screen.right() - 320.0).max(screen.left() + 8.0),
            ),
            anchor.y.clamp(
                screen.top() + 8.0,
                (screen.bottom() - 370.0).max(screen.top() + 8.0),
            ),
        );
        egui::Window::new("Choose date")
            .open(&mut open)
            .fixed_pos(position)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("‹").on_hover_text("Previous month").clicked() {
                        previous_month(&mut picker);
                    }
                    ui.add_sized(
                        [132.0, 22.0],
                        egui::Label::new(
                            RichText::new(format!("{} {}", month_name(picker.month), picker.year))
                                .strong(),
                        ),
                    );
                    if ui.button("›").on_hover_text("Next month").clicked() {
                        next_month(&mut picker);
                    }
                    if ui.button("Today").clicked() {
                        chosen = Some(pgn_from_ymd(
                            (players::today() / 10_000) as i32,
                            (players::today() / 100) % 100,
                            players::today() % 100,
                        ));
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Date").small().weak());
                    ui.add_sized(
                        [120.0, 20.0],
                        egui::TextEdit::singleline(&mut picker.manual)
                            .hint_text("YYYY.MM.DD")
                            .font(FontId::monospace(12.5)),
                    );
                    if ui.button("Apply").clicked() {
                        chosen = Some(picker.manual.trim().to_string());
                    }
                });
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                        ui.add_sized(
                            [34.0, 18.0],
                            egui::Label::new(
                                RichText::new(day)
                                    .small()
                                    .weak()
                                    .color(Color32::from_gray(145)),
                            ),
                        );
                    }
                });
                let days = days_in_month(picker.year, picker.month);
                let first = weekday_monday(picker.year, picker.month, 1);
                let mut day = 1u32;
                for week in 0..6 {
                    ui.horizontal(|ui| {
                        for slot in 0..7 {
                            let index = week * 7 + slot;
                            if index < first || day > days {
                                ui.add_sized([34.0, 24.0], egui::Label::new(""));
                                continue;
                            }
                            let label = day.to_string();
                            let selected = picker.manual.trim()
                                == pgn_from_ymd(picker.year, picker.month, day);
                            if ui
                                .add_sized(
                                    [34.0, 24.0],
                                    egui::Button::new(RichText::new(label).monospace().color(
                                        if selected {
                                            Color32::from_rgb(0x19, 0x1B, 0x1D)
                                        } else {
                                            Color32::from_gray(220)
                                        },
                                    ))
                                    .fill(if selected {
                                        GOLD
                                    } else {
                                        Color32::from_rgb(0x2A, 0x2E, 0x32)
                                    })
                                    .corner_radius(4.0),
                                )
                                .clicked()
                            {
                                chosen = Some(pgn_from_ymd(picker.year, picker.month, day));
                            }
                            day += 1;
                        }
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear date").clicked() {
                        chosen = Some("????.??.??".to_string());
                    }
                });
            });

        if let Some(date) = chosen {
            if let Some(game) = self.current.as_mut() {
                game.set_header("Date", date);
                self.dirty = true;
                self.status = "Date updated.".to_string();
            }
            self.date_picker = None;
            self.date_picker_anchor = None;
        } else if open {
            self.date_picker = Some(picker);
        } else {
            self.date_picker = None;
            self.date_picker_anchor = None;
        }
    }

    fn reset_branch_state(&mut self) {
        self.branch_choice = None;
        self.branch_recency.clear();
        self.branch_clock = 0;
        self.board_tag_visible = false;
        self.forced_tag_visibility.clear();
    }

    fn branch_choice_dialog(&mut self, ctx: &egui::Context) {
        let Some((parent, options, selected)) = self
            .branch_choice
            .as_ref()
            .map(|choice| (choice.parent, choice.options.clone(), choice.selected))
        else {
            return;
        };
        if self.node != parent {
            self.branch_choice = None;
            return;
        }
        let Some(game) = self.game() else {
            self.branch_choice = None;
            return;
        };
        let labels: Vec<String> = options
            .iter()
            .map(|&node| branch_option_label(game, node))
            .collect();
        let mut choose = None;
        let mut cancel = false;
        egui::Window::new("Choose a continuation")
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .show(ctx, |ui| {
                ui.set_min_width(330.0);
                ui.label(
                    RichText::new("This position has more than one continuation.")
                        .small()
                        .weak(),
                );
                ui.add_space(5.0);
                for (index, label) in labels.iter().enumerate() {
                    let text = RichText::new(format!("{}.  {label}", index + 1)).monospace();
                    let response = ui.selectable_label(index == selected, text);
                    if response.clicked() {
                        choose = Some(index);
                    }
                }
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                    ui.label(
                        RichText::new("↑/↓ choose · Enter or → play · 1–9 pick · Esc cancel")
                            .small()
                            .weak(),
                    );
                });
                ui.label(
                    RichText::new("Main line first · variations by most recent use")
                        .small()
                        .weak(),
                );
            });
        if let Some(index) = choose {
            let node = options.get(index).copied();
            self.branch_choice = None;
            if let Some(node) = node {
                self.goto(node);
            }
        } else if cancel {
            self.branch_choice = None;
        }
    }

    fn next_move(&mut self) {
        let Some(game) = self.game() else {
            return;
        };
        let children = game.nodes[self.node].children.clone();
        match children.as_slice() {
            [] => {}
            [child] => self.goto(*child),
            _ => self.open_branch_choice(),
        }
    }

    /// Offer the continuations of the current position. The mainline is
    /// selected first; variations follow in descending session-recency order.
    fn open_branch_choice(&mut self) {
        let Some(game) = self.game() else {
            return;
        };
        let parent = self.node;
        let children = game.nodes[parent].children.clone();
        if children.len() < 2 {
            if let Some(&child) = children.first() {
                self.goto(child);
            }
            return;
        }
        let mut variations = children[1..].to_vec();
        variations.sort_by(|a, b| {
            let recency = |node: &usize| self.branch_recency.get(node).copied().unwrap_or(0);
            recency(b).cmp(&recency(a))
        });
        let mut options = Vec::with_capacity(children.len());
        options.push(children[0]);
        options.extend(variations);
        self.branch_choice = Some(BranchChoice {
            parent,
            options,
            selected: 0,
        });
    }

    fn prev_move(&mut self) {
        if let Some(g) = self.game()
            && let Some(parent) = g.nodes[self.node].parent
        {
            self.goto(parent);
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
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if extension == "cbv" {
            self.open_rx = None;
            self.open_job = Some(path.to_path_buf());
            self.status = format!(
                "Opening {} — the first open extracts its database files into the Brett cache…",
                path.display()
            );
            let (tx, rx) = mpsc::channel();
            let ctx = self.ctx.clone();
            let archive = path.to_path_buf();
            std::thread::spawn(move || {
                let progress_tx = tx.clone();
                let progress_ctx = ctx.clone();
                let result = CbhLibrary::open_cbv_with_progress(&archive, move |member| {
                    let _ = progress_tx.send(OpenMessage::Progress(format!(
                        "Extracting ChessBase archive: {member}"
                    )));
                    progress_ctx.request_repaint();
                })
                .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
                .map_err(|err| format!("{err:#}"));
                let _ = tx.send(OpenMessage::Done(result));
                ctx.request_repaint();
            });
            self.open_rx = Some(rx);
            return;
        }
        match open_library(path) {
            Ok(library) => self.install_library(library, path),
            Err(err) => self.status = format!("Could not read {}: {err}", path.display()),
        }
    }

    fn install_library(&mut self, library: Arc<dyn GameLibrary>, path: &std::path::Path) {
        let count = library.len();
        let indexed_in = library.indexed_in();
        self.players = PlayerDirectory::default();
        self.players_rx = None;
        self.players_job = None;
        self.players_status = "No players indexed.".to_string();
        self.session = crate::session::GameList::from_library(library.clone());
        self.library = Some(library.clone());
        self.edited.clear();
        self.dirty = false;
        self.session_dirty = false;
        self.database_save_path = None;
        self.current_slot = None;
        self.current = None;
        self.print_diagrams.clear();
        self.clear_engine_results();
        self.goto(0);
        self.left_tab = LeftTab::Games;
        self.focus_right_tab(RightTab::Moves);
        self.selected_player = None;
        self.reset_wiki();
        self.player_filter.clear();
        self.player_book = None;
        self.player_book_rx = None;
        self.player_book_job = None;
        self.player_book_status = "Select a player to build their preparation book.".to_string();
        self.prep = None;
        self.recent_preview = None;
        self.cloud = None;
        self.cloud_cache.clear();
        self.cloud_rx = None;
        self.cloud_job = None;
        self.explorer_rx = None;
        self.explorer_job = None;
        self.cloud_pending = None;
        self.online_game_rx = None;
        self.online_game_job = None;
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
        if let Some(id) = self.session.id_at(0) {
            self.select_slot(id);
        }
        if count > LARGE_DATABASE_GAMES {
            self.start_player_directory(library, path.to_path_buf());
        } else {
            self.players = PlayerDirectory::build(library.as_ref());
            self.players_status = format!("{} players indexed.", games_label(self.players.len()));
        }
        if count <= AUTO_INDEX_MAX_GAMES {
            self.start_index();
        } else {
            self.index = None;
            self.index_rx = None;
            self.index_job = Some(path.to_path_buf());
            self.index_status = format!(
                "{} from {} · position index not built automatically",
                games_label(count),
                path.display()
            );
            self.status = format!(
                "{} · open Tree to build the position index when needed.",
                self.index_status
            );
        }
        self.remember_database_dir(path);
    }

    /// Build the player directory in the background for a very large database.
    fn start_player_directory(&mut self, library: Arc<dyn GameLibrary>, path: PathBuf) {
        if self.players_job.as_ref() == Some(&path) && self.players_rx.is_some() {
            return;
        }
        self.players = PlayerDirectory::default();
        self.selected_player = None;
        self.reset_wiki();
        self.players_job = Some(path.clone());
        self.players_status = format!("Indexing players in {}…", path.display());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let directory = PlayerDirectory::build(library.as_ref());
            let _ = tx.send(PlayerDirectoryMessage::Done(directory));
            ctx.request_repaint();
        });
        self.players_rx = Some(rx);
    }

    fn poll_player_directory(&mut self) {
        let Some(rx) = self.players_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(PlayerDirectoryMessage::Done(directory)) => {
                self.players_status = format!("{} players indexed.", games_label(directory.len()));
                self.players = directory;
                self.players_job = None;
            }
            Err(TryRecvError::Empty) => {
                self.players_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.players_status = "Player indexing stopped unexpectedly.".to_string();
                self.players_job = None;
            }
        }
    }

    fn poll_open(&mut self) {
        let Some(rx) = self.open_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(OpenMessage::Progress(message)) => self.status = message,
                Ok(OpenMessage::Done(Ok(library))) => {
                    let path = self
                        .open_job
                        .take()
                        .unwrap_or_else(|| library.path().to_path_buf());
                    self.install_library(library, &path);
                    keep = false;
                    break;
                }
                Ok(OpenMessage::Done(Err(err))) => {
                    self.status = format!(
                        "Could not open {}: {err}",
                        self.open_job
                            .take()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "ChessBase archive".to_string())
                    );
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.status = "Opening the database stopped unexpectedly.".to_string();
                    self.open_job = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.open_rx = Some(rx);
        }
    }

    fn open_database_view(&mut self, path: &std::path::Path) {
        if self
            .library
            .as_ref()
            .is_some_and(|library| library.path() == path)
        {
            self.status = format!("{} is already open in the main view.", path.display());
            return;
        }
        if self.views.iter().any(|view| view.path == path) {
            self.views_open = true;
            self.status = format!("{} is already open in Database views.", path.display());
            return;
        }
        match open_library(path) {
            Ok(library) => {
                let count = library.len();
                let session = crate::session::GameList::from_library(library.clone());
                self.views.push(DatabaseView {
                    path: path.to_path_buf(),
                    library,
                    session,
                    filter: String::new(),
                });
                self.views_open = true;
                self.status = format!("Opened {} as a database view.", games_label(count));
                self.remember_database_dir(path);
            }
            Err(err) => self.status = format!("Could not open {}: {err}", path.display()),
        }
    }

    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        if self.library.is_none() && self.session.is_empty() {
            self.load_file(path);
        } else {
            self.pending_open_choice = Some(PendingOpenChoice {
                path: path.to_path_buf(),
            });
        }
    }

    fn finish_game_drop(&mut self, pending: PendingGameDrop, insert: bool) {
        let insert_at = insert.then_some(pending.insert_at);
        match pending.target {
            DropTarget::Main => {
                let id = self.session.insert_game(pending.game, insert_at);
                self.session_dirty = true;
                self.select_slot(id);
                self.status = format!(
                    "{} added {}.",
                    pending.label,
                    if insert {
                        "at the drop position"
                    } else {
                        "at the end of the list"
                    }
                );
            }
            DropTarget::View(index) => {
                if let Some(view) = self.views.get_mut(index) {
                    view.session.insert_game(pending.game, insert_at);
                    self.status = format!(
                        "{} added to {} {}.",
                        pending.label,
                        view.name(),
                        if insert {
                            "at the drop position"
                        } else {
                            "at the end"
                        }
                    );
                }
            }
        }
    }

    fn finish_merge(&mut self, pending: PendingMerge) {
        match pending.target {
            DropTarget::Main => match self.session.merge_library(pending.library, &self.edited) {
                Ok(report) => {
                    if report.added > 0 {
                        self.session_dirty = true;
                    }
                    self.status = format!(
                        "Merged {}: {} added, {} duplicates skipped.",
                        pending.label,
                        games_label(report.added),
                        report.skipped
                    );
                }
                Err(err) => self.status = format!("Merge failed: {err:#}"),
            },
            DropTarget::View(index) => {
                let Some(view) = self.views.get_mut(index) else {
                    self.status = "The target database view is gone.".to_string();
                    return;
                };
                let name = view.name();
                match view
                    .session
                    .merge_library(pending.library, &std::collections::HashMap::new())
                {
                    Ok(report) => {
                        self.status = format!(
                            "Merged {} into {}: {} added, {} duplicates skipped.",
                            pending.label,
                            name,
                            games_label(report.added),
                            report.skipped
                        );
                    }
                    Err(err) => self.status = format!("Merge failed: {err:#}"),
                }
            }
        }
    }

    fn current_database_format(&self) -> Option<DatabaseFormat> {
        self.database_save_path
            .as_deref()
            .map(DatabaseFormat::from_path)
            .or_else(|| {
                self.library
                    .as_ref()
                    .map(|library| DatabaseFormat::from_path(library.path()))
            })
    }

    fn database_stem(&self) -> String {
        self.library
            .as_ref()
            .map(|library| library.path())
            .or(self.database_save_path.as_deref())
            .and_then(Path::file_stem)
            .map(|stem| stem.to_string_lossy().trim().to_string())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "Brett Database".to_string())
    }

    fn default_database_save_path(&self, format: DatabaseFormat) -> PathBuf {
        // Once this session has a writable target in the chosen format, use
        // it again instead of growing a "-saved" suffix on every Save As.
        if let Some(path) = self
            .database_save_path
            .as_deref()
            .filter(|path| DatabaseFormat::from_path(path) == format)
        {
            return path.to_path_buf();
        }
        let directory = self
            .last_database_dir
            .clone()
            .unwrap_or_else(default_database_dir);
        database_save_default_path(
            &self.database_stem(),
            self.library.as_ref().map(|library| library.path()),
            &directory,
            format,
        )
    }

    fn finish_saved_database(&mut self, path: &Path, close_after: bool) {
        self.database_save_path = Some(path.to_path_buf());
        self.session_dirty = false;
        if self.current_slot.is_some() {
            self.dirty = false;
        }
        self.remember_database_dir(path);
        if close_after {
            self.pending_window_close = true;
        }
    }

    fn start_format_save(
        &mut self,
        format: DatabaseFormat,
        target: Option<PathBuf>,
        close_after: bool,
    ) {
        match format {
            DatabaseFormat::Pgn => self.start_pgn_save(target, close_after),
            DatabaseFormat::Scid => self.start_scid_save(target, close_after),
            DatabaseFormat::Cbh => self.start_cbh_save(target, close_after),
            DatabaseFormat::TwoCbh => self.start_two_cbh_save(target, close_after),
        }
    }

    /// Save in the current database's format. A new database opens the format
    /// chooser; an existing writable target saves directly.
    fn save_database_now(&mut self, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        if let Some(format) = self.current_database_format() {
            if let Some(path) = self
                .database_save_path
                .clone()
                .filter(|path| DatabaseFormat::from_path(path) == format)
            {
                self.start_format_save(format, Some(path), close_after);
                return;
            }
            let default = self.default_database_save_path(format);
            let Some(path) = save_format_dialog(format, &default) else {
                self.status = "Save cancelled.".to_string();
                return;
            };
            self.start_format_save(format, Some(path), close_after);
            return;
        }
        self.open_save_database_dialog(false, close_after, DatabaseFormat::Pgn);
    }

    fn save_database_as_now(&mut self, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        let format = self
            .current_database_format()
            .filter(|format| format.is_writable())
            .unwrap_or(DatabaseFormat::Pgn);
        self.open_save_database_dialog(true, close_after, format);
    }

    fn open_save_database_dialog(
        &mut self,
        as_mode: bool,
        close_after: bool,
        format: DatabaseFormat,
    ) {
        self.save_database_as = as_mode;
        self.save_database_format = format;
        self.save_database_path = self
            .default_database_save_path(format)
            .display()
            .to_string();
        self.save_database_close_after = close_after;
        self.save_database_error.clear();
        self.save_database_dialog_open = true;
    }

    fn refresh_save_database_path(&mut self, previous: DatabaseFormat) {
        let path = PathBuf::from(self.save_database_path.trim());
        if path.as_os_str().is_empty() {
            self.save_database_path = self
                .default_database_save_path(self.save_database_format)
                .display()
                .to_string();
        } else if path.extension().is_none()
            || path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case(previous.extension()))
        {
            self.save_database_path = path
                .with_extension(self.save_database_format.extension())
                .display()
                .to_string();
        }
    }

    fn save_database_dialog(&mut self, ctx: &egui::Context) {
        if !self.save_database_dialog_open {
            return;
        }
        let previous_format = self.save_database_format;
        let title = if self.save_database_as {
            "Save database as"
        } else {
            "Save database"
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let mut browse = false;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(560.0);
                ui.label(RichText::new("Format").strong());
                for format in DatabaseFormat::WRITABLE {
                    ui.radio_value(&mut self.save_database_format, format, format.label());
                }
                ui.add_space(6.0);
                ui.label("File");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_database_path)
                            .desired_width(440.0),
                    );
                    if ui.button("Browse…").clicked() {
                        browse = true;
                    }
                });
                if self.save_database_format == DatabaseFormat::TwoCbh {
                    ui.label(
                        RichText::new(
                            "2CBH writes moves, variations, and metadata. Comments, NAGs, clocks, and evaluations are not written yet.",
                        )
                        .small()
                        .weak(),
                    );
                }
                if !self.save_database_error.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.save_database_error).small().color(GOLD));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if previous_format != self.save_database_format {
            self.refresh_save_database_path(previous_format);
        }
        if browse {
            let default = PathBuf::from(self.save_database_path.trim());
            let default = if default.as_os_str().is_empty() {
                self.default_database_save_path(self.save_database_format)
            } else {
                default
            };
            if let Some(path) = save_format_dialog(self.save_database_format, &default) {
                self.save_database_path = path.display().to_string();
            }
        }
        if save {
            let mut path = PathBuf::from(self.save_database_path.trim());
            if path.as_os_str().is_empty() {
                self.save_database_error = "Choose a file path.".to_string();
            } else {
                if !path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case(self.save_database_format.extension())
                }) {
                    path.set_extension(self.save_database_format.extension());
                }
                let format = self.save_database_format;
                let close_after = self.save_database_close_after;
                self.save_database_dialog_open = false;
                self.save_database_close_after = false;
                self.save_database_error.clear();
                self.start_format_save(format, Some(path), close_after);
            }
        } else if cancel || !open {
            self.save_database_dialog_open = false;
            self.save_database_close_after = false;
            self.save_database_error.clear();
            self.status = "Save cancelled.".to_string();
        }
    }

    fn start_pgn_save(&mut self, target: Option<PathBuf>, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        let mut path = match target {
            Some(path) => path,
            None => {
                let default = self.default_database_save_path(DatabaseFormat::Pgn);
                let Some(path) = save_format_dialog(DatabaseFormat::Pgn, &default) else {
                    self.status = "Save cancelled.".to_string();
                    return;
                };
                path
            }
        };
        if path.extension().is_none() {
            path.set_extension("pgn");
        }
        if is_read_only_export_target(&path) {
            self.status =
                "That folder is read-only in Brett. Choose a path outside the source collection."
                    .to_string();
            return;
        }
        if self.pgn_export_would_overwrite_source(&path) {
            self.status =
                "Refusing to overwrite the open source database. Choose a new file.".to_string();
            return;
        }

        let games = self.session.len();
        let estimate = self.pgn_export_estimate();
        if estimate >= LARGE_PGN_BYTES {
            self.pending_pgn_export = Some(PendingPgnExport {
                path,
                games,
                estimate,
                close_after,
            });
            self.status = format!("Confirm saving {} games.", games_label(games));
        } else {
            self.write_pgn_export(&path, close_after);
        }
    }

    fn pgn_export_estimate(&self) -> u64 {
        let source_size = self
            .library
            .as_ref()
            .and_then(|library| std::fs::metadata(library.path()).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let text_estimate = (self.session.len() as u64).saturating_mul(8_000);
        source_size.max(text_estimate)
    }

    fn pgn_export_would_overwrite_source(&self, path: &std::path::Path) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let source = library.path();
        if path == source {
            return true;
        }
        matches!(
            (std::fs::canonicalize(path), std::fs::canonicalize(source)),
            (Ok(target), Ok(source)) if target == source
        )
    }

    fn write_pgn_export(&mut self, path: &std::path::Path, close_after: bool) {
        self.persist_current_edit();
        match self.try_write_pgn_export(path) {
            Ok(games) => {
                self.status = format!("Saved {} to {}.", games_label(games), path.display());
                self.finish_saved_database(path, close_after);
            }
            Err(err) => {
                self.status = format!("PGN save failed: {err}");
            }
        }
    }

    fn try_write_pgn_export(&self, path: &std::path::Path) -> Result<usize, String> {
        crate::session::ensure_unique_ids(&self.session)
            .map_err(|err| format!("checking the session: {err:#}"))?;

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("brett-export.pgn");
        let temp = path.with_file_name(format!(".{file_name}.tmp"));
        let file = std::fs::File::create(&temp)
            .map_err(|err| format!("creating {}: {err}", temp.display()))?;
        let mut writer = std::io::BufWriter::new(file);
        let ids: Vec<usize> = self.session.slots().iter().map(|slot| slot.id).collect();
        let mut written = 0usize;

        for id in ids {
            let game = match self.session.load(id, &self.edited) {
                Ok(game) => game,
                Err(err) => {
                    let _ = std::fs::remove_file(&temp);
                    return Err(format!("loading game row {id}: {err:#}"));
                }
            };
            if let Err(err) = pgn::write_game(&mut writer, &game) {
                let _ = std::fs::remove_file(&temp);
                return Err(format!("writing {}: {err}", path.display()));
            }
            written += 1;
        }

        if let Err(err) = std::io::Write::flush(&mut writer) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("flushing {}: {err}", temp.display()));
        }
        drop(writer);
        if let Err(err) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!(
                "moving the completed export to {}: {err}",
                path.display()
            ));
        }
        Ok(written)
    }

    fn pgn_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_pgn_export.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Large PGN export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "This will write {} to:",
                    games_label(pending.games)
                ));
                ui.label(RichText::new(pending.path.display().to_string()).monospace());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Estimated size: {}.",
                        human_bytes(pending.estimate)
                    ))
                    .strong(),
                );
                ui.label(
                    RichText::new(
                        "PGN is plain text and can be much larger than a compressed SCID or classic CBH database. The export may take a while.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Export anyway").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_pgn_export = None;
            self.write_pgn_export(&pending.path, pending.close_after);
        } else if cancel || !open {
            self.pending_pgn_export = None;
            self.status = "Save cancelled.".to_string();
        }
    }

    fn scid_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_scid_export.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Replace SCID database")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "This will write {} to:",
                    games_label(pending.games)
                ));
                ui.label(RichText::new(pending.path.display().to_string()).monospace());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "A SCID database is three files. These existing files will be replaced together:",
                    )
                    .strong(),
                );
                for path in &pending.existing {
                    ui.label(RichText::new(path.display().to_string()).monospace());
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "The old triple is kept aside until the new one is complete, then removed.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Replace database").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_scid_export = None;
            self.write_scid_export(&pending.path, pending.close_after);
        } else if cancel || !open {
            self.pending_scid_export = None;
            self.status = "Save cancelled.".to_string();
        }
    }

    fn start_scid_save(&mut self, target: Option<PathBuf>, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        let mut path = match target {
            Some(path) => path,
            None => {
                let default = self.default_database_save_path(DatabaseFormat::Scid);
                let Some(path) = save_format_dialog(DatabaseFormat::Scid, &default) else {
                    self.status = "Save cancelled.".to_string();
                    return;
                };
                path
            }
        };
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("si4"))
        {
            path.set_extension("si4");
        }
        if is_read_only_export_target(&path) {
            self.status =
                "That folder is read-only in Brett. Choose a path outside the source collection."
                    .to_string();
            return;
        }
        if self.scid_export_would_overwrite_source(&path) {
            self.status =
                "Refusing to overwrite the open source database. Choose a new file.".to_string();
            return;
        }

        let existing: Vec<PathBuf> = scid_triple(&path)
            .into_iter()
            .filter(|candidate| candidate.exists())
            .collect();
        let games = self.session.len();
        if existing.is_empty() {
            self.write_scid_export(&path, close_after);
        } else {
            self.pending_scid_export = Some(PendingScidExport {
                path,
                games,
                existing,
                close_after,
            });
            self.status = format!(
                "Confirm replacing the existing SCID database for {}.",
                games_label(games)
            );
        }
    }

    fn scid_export_would_overwrite_source(&self, path: &std::path::Path) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let source = library.path();
        let mut protected = vec![source.to_path_buf()];
        for extension in ["si4", "sn4", "sg4", "si3", "sn3", "sg3"] {
            protected.push(source.with_extension(extension));
        }
        scid_triple(path).iter().any(|target| {
            protected
                .iter()
                .any(|source| same_file_path(target, source))
        })
    }

    fn write_scid_export(&mut self, path: &std::path::Path, close_after: bool) {
        self.persist_current_edit();
        match self.try_write_scid_export(path) {
            Ok(report) => {
                self.status = format!(
                    "Saved {} to {} ({}).",
                    games_label(report.games),
                    path.display(),
                    human_bytes(report.bytes)
                );
                self.finish_saved_database(path, close_after);
            }
            Err(err) => {
                self.status = format!("SCID save failed: {err}");
            }
        }
    }

    fn try_write_scid_export(
        &self,
        path: &std::path::Path,
    ) -> Result<crate::scid::ScidWriteReport, String> {
        crate::session::ensure_unique_ids(&self.session)
            .map_err(|err| format!("checking the session: {err:#}"))?;

        let ids: Vec<usize> = self.session.slots().iter().map(|slot| slot.id).collect();
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "Brett export".to_string());
        let description = format!("Brett export: {stem}");
        crate::scid::write_database(
            path,
            &description,
            ids.into_iter()
                .map(|id| self.session.load(id, &self.edited)),
        )
        .map_err(|err| format!("{err:#}"))
    }

    fn cbh_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_cbh_export.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Replace CBH database")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "This will write {} to:",
                    games_label(pending.games)
                ));
                ui.label(RichText::new(pending.path.display().to_string()).monospace());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "A classic ChessBase database is a multi-file set. These existing companion files will be replaced together:",
                    )
                    .strong(),
                );
                for path in &pending.existing {
                    ui.label(RichText::new(path.display().to_string()).monospace());
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "The old companion files are kept aside until the new set is complete, then removed.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Replace database").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_cbh_export = None;
            self.write_cbh_export(&pending.path, pending.close_after);
        } else if cancel || !open {
            self.pending_cbh_export = None;
            self.status = "Save cancelled.".to_string();
        }
    }

    fn start_cbh_save(&mut self, target: Option<PathBuf>, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        let mut path = match target {
            Some(path) => path,
            None => {
                let default = self.default_database_save_path(DatabaseFormat::Cbh);
                let Some(path) = save_format_dialog(DatabaseFormat::Cbh, &default) else {
                    self.status = "Save cancelled.".to_string();
                    return;
                };
                path
            }
        };
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cbh"))
        {
            path.set_extension("cbh");
        }
        if is_read_only_export_target(&path) {
            self.status =
                "That folder is read-only in Brett. Choose a path outside the source collection."
                    .to_string();
            return;
        }
        if self.cbh_export_would_overwrite_source(&path) {
            self.status =
                "Refusing to overwrite the open source database. Choose a new file.".to_string();
            return;
        }

        let existing = crate::cbh::existing_companion_files(&path);
        let games = self.session.len();
        if existing.is_empty() {
            self.write_cbh_export(&path, close_after);
        } else {
            self.pending_cbh_export = Some(PendingCbhExport {
                path,
                games,
                existing,
                close_after,
            });
            self.status = format!(
                "Confirm replacing the existing CBH database for {}.",
                games_label(games)
            );
        }
    }

    fn cbh_export_would_overwrite_source(&self, path: &std::path::Path) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let source = library.path();
        let mut protected = vec![source.to_path_buf()];
        for extension in crate::cbh::ALL_EXTENSIONS {
            protected.push(source.with_extension(extension));
        }
        for extension in crate::cbh::ALL_EXTENSIONS {
            let target = path.with_extension(extension);
            if protected
                .iter()
                .any(|source_file| same_file_path(&target, source_file))
            {
                return true;
            }
        }
        false
    }

    fn write_cbh_export(&mut self, path: &std::path::Path, close_after: bool) {
        self.persist_current_edit();
        match self.try_write_cbh_export(path) {
            Ok(report) => {
                self.status = format!(
                    "Saved {} to {} ({}).",
                    games_label(report.games),
                    path.display(),
                    human_bytes(report.bytes)
                );
                self.finish_saved_database(path, close_after);
            }
            Err(err) => {
                self.status = format!("CBH save failed: {err}");
            }
        }
    }

    fn try_write_cbh_export(
        &self,
        path: &std::path::Path,
    ) -> Result<crate::cbh::CbhWriteReport, String> {
        crate::session::ensure_unique_ids(&self.session)
            .map_err(|err| format!("checking the session: {err:#}"))?;

        let ids: Vec<usize> = self.session.slots().iter().map(|slot| slot.id).collect();
        crate::cbh::write_database(
            path,
            ids.into_iter()
                .map(|id| self.session.load(id, &self.edited)),
        )
        .map_err(|err| format!("{err:#}"))
    }

    fn two_cbh_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_two_cbh_export.clone() else {
            return;
        };
        let title = if pending.existing.is_empty() {
            "Write 2CBH database"
        } else {
            "Replace 2CBH database"
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "This will write {} to:",
                    games_label(pending.games)
                ));
                ui.label(RichText::new(pending.path.display().to_string()).monospace());
                ui.add_space(6.0);
                if pending.annotations {
                    ui.label(
                        RichText::new(
                            "Comments, NAGs, clocks, and evaluations are not written by the first 2CBH writer. They will be lost in this export.",
                        )
                        .strong(),
                    );
                }
                if !pending.existing.is_empty() {
                    ui.label(
                        RichText::new(
                            "A 2CBH database is a four-file set. These existing companion files will be replaced together:",
                        )
                        .strong(),
                    );
                    for path in &pending.existing {
                        ui.label(RichText::new(path.display().to_string()).monospace());
                    }
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Stale .2lcd/.2lgd indexes are removed so ChessBase rebuilds them.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Write database").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_two_cbh_export = None;
            self.write_two_cbh_export(&pending.path, pending.annotations, pending.close_after);
        } else if cancel || !open {
            self.pending_two_cbh_export = None;
            self.status = "Save cancelled.".to_string();
        }
    }

    fn start_two_cbh_save(&mut self, target: Option<PathBuf>, close_after: bool) {
        if self.session.is_empty() {
            self.status = "There are no games to save.".to_string();
            return;
        }
        self.persist_current_edit();
        let mut path = match target {
            Some(path) => path,
            None => {
                let default = self.default_database_save_path(DatabaseFormat::TwoCbh);
                let Some(path) = save_format_dialog(DatabaseFormat::TwoCbh, &default) else {
                    self.status = "Save cancelled.".to_string();
                    return;
                };
                path
            }
        };
        let core_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                crate::twocbh::CORE_EXTENSIONS
                    .iter()
                    .any(|core| extension.eq_ignore_ascii_case(core))
            });
        if path.extension().is_none() || core_extension {
            path.set_extension("2cbh");
        }
        if is_read_only_export_target(&path) {
            self.status =
                "That folder is read-only in Brett. Choose a path outside the source collection."
                    .to_string();
            return;
        }
        if self.two_cbh_export_would_overwrite_source(&path) {
            self.status =
                "Refusing to overwrite the open source database. Choose a new file.".to_string();
            return;
        }

        let existing = crate::twocbh::existing_companion_files(&path);
        let annotations = match self.session_has_annotations() {
            Ok(annotations) => annotations,
            Err(err) => {
                self.status = format!("2CBH save failed: {err}");
                return;
            }
        };
        let games = self.session.len();
        if existing.is_empty() && !annotations {
            self.write_two_cbh_export(&path, false, close_after);
        } else {
            self.pending_two_cbh_export = Some(PendingTwoCbhExport {
                path,
                games,
                existing,
                annotations,
                close_after,
            });
            self.status = format!(
                "Confirm writing the 2CBH database for {}.",
                games_label(games)
            );
        }
    }

    fn session_has_annotations(&self) -> Result<bool, String> {
        for slot in self.session.slots() {
            let game = self
                .session
                .load(slot.id, &self.edited)
                .map_err(|err| format!("{err:#}"))?;
            if crate::twocbh::has_unsupported_annotations(&game) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn two_cbh_export_would_overwrite_source(&self, path: &std::path::Path) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let source = library.path();
        if same_file_path(path, source) {
            return true;
        }
        if DatabaseFormat::from_path(source) != DatabaseFormat::TwoCbh {
            return false;
        }
        for extension in crate::twocbh::ALL_EXTENSIONS {
            if same_file_path(
                &path.with_extension(extension),
                &source.with_extension(extension),
            ) {
                return true;
            }
        }
        false
    }

    fn write_two_cbh_export(
        &mut self,
        path: &std::path::Path,
        allow_annotation_loss: bool,
        close_after: bool,
    ) {
        self.persist_current_edit();
        match self.try_write_two_cbh_export(path, allow_annotation_loss) {
            Ok(report) => {
                self.status = format!(
                    "Saved {} to {} ({}).",
                    games_label(report.games),
                    path.display(),
                    human_bytes(report.bytes)
                );
                self.finish_saved_database(path, close_after);
            }
            Err(err) => {
                self.status = format!("2CBH save failed: {err}");
            }
        }
    }

    fn try_write_two_cbh_export(
        &self,
        path: &std::path::Path,
        allow_annotation_loss: bool,
    ) -> Result<crate::twocbh::TwoCbhWriteReport, String> {
        crate::session::ensure_unique_ids(&self.session)
            .map_err(|err| format!("checking the session: {err:#}"))?;

        let ids: Vec<usize> = self.session.slots().iter().map(|slot| slot.id).collect();
        crate::twocbh::write_database_with_options(
            path,
            crate::twocbh::TwoCbhWriteOptions {
                allow_annotation_loss,
            },
            ids.into_iter()
                .map(|id| self.session.load(id, &self.edited)),
        )
        .map_err(|err| format!("{err:#}"))
    }

    fn start_print_export(&mut self) {
        if self.current.is_none() {
            self.status = "Open a game first.".to_string();
            return;
        }
        self.print_export_path = self.default_print_export_path().display().to_string();
        self.print_export_include_final = true;
        self.print_export_compile_pdf = false;
        self.print_export_status.clear();
        self.print_export_open = true;
    }

    fn default_print_export_path(&self) -> PathBuf {
        default_print_path(self.last_print_dir.as_deref(), self.current.as_ref())
    }

    fn print_source_label(&self) -> String {
        self.library
            .as_ref()
            .and_then(|library| library.path().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Brett".to_string())
    }

    fn export_current_game_as_typst(&mut self) {
        let Some(game) = self.current.clone() else {
            self.print_export_status = "Open a game first.".to_string();
            return;
        };
        let raw = self.print_export_path.trim().to_string();
        if raw.is_empty() {
            self.print_export_status = "Choose a .typ output path.".to_string();
            return;
        }
        let mut path = PathBuf::from(raw);
        if path.extension().is_none() {
            path.set_extension("typ");
        }
        if is_read_only_export_target(&path) {
            self.print_export_status =
                "That folder is read-only in Brett. Choose a path outside the source collection."
                    .to_string();
            return;
        }

        let meta = crate::print::PrintMeta {
            source: self.print_source_label(),
            owner: print_owner(),
            date: print_date(),
        };
        let document = crate::print::build_document(
            &game,
            meta,
            &self.print_diagrams,
            self.print_export_include_final,
        );
        let bundle = match crate::print::typst::write_bundle(&path, &document) {
            Ok(bundle) => bundle,
            Err(err) => {
                self.print_export_status = format!("Could not write the Typst source: {err:#}");
                return;
            }
        };
        self.remember_print_dir(&path);
        self.print_export_path = path.display().to_string();
        let mut status = format!(
            "Wrote {} and {} diagram(s).",
            bundle.source.display(),
            bundle.diagrams.len()
        );
        if self.print_export_compile_pdf {
            let pdf = path.with_extension("pdf");
            match crate::print::typst::compile_pdf(&bundle.source, &pdf) {
                Ok(()) => status.push_str(&format!(" Compiled {}.", pdf.display())),
                Err(err) => status.push_str(&format!(" PDF not compiled: {err:#}")),
            }
        }
        self.status = status.clone();
        self.print_export_status = status;
    }

    fn print_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.print_export_open {
            return;
        }
        let has_current = self.current.is_some();
        let game_label = self
            .current
            .as_ref()
            .map(|game| format!("{} – {}", game.white(), game.black()))
            .unwrap_or_else(|| "No game open".to_string());
        let mut open = true;
        let mut export = false;
        let mut cancel = false;
        let mut browse = false;
        egui::Window::new("Export current game as Typst")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(520.0);
                ui.label(RichText::new(game_label).strong());
                ui.add_space(6.0);
                ui.label("Typst source file");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.print_export_path)
                            .desired_width(390.0),
                    );
                    if ui.button("Browse…").clicked() {
                        browse = true;
                    }
                });
                ui.label(
                    RichText::new(
                        "The board diagrams are written to a folder named after the file.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.print_export_include_final,
                    "Include the final position diagram",
                );
                ui.checkbox(
                    &mut self.print_export_compile_pdf,
                    "Compile a PDF with Typst (optional)",
                );
                ui.label(
                    RichText::new("Uses BRETT_TYPST or the local typst binary.")
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_current, egui::Button::new("Export"))
                        .clicked()
                    {
                        export = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if !self.print_export_status.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&self.print_export_status).small().color(GOLD));
                }
            });
        if browse {
            let default = PathBuf::from(self.print_export_path.trim());
            let default = if default.as_os_str().is_empty() {
                self.default_print_export_path()
            } else {
                default
            };
            if let Some(path) = save_print_dialog(&default) {
                self.remember_print_dir(&path);
                self.print_export_path = path.display().to_string();
            }
        }
        if export {
            self.export_current_game_as_typst();
        } else if cancel || !open {
            self.print_export_open = false;
            self.print_export_status.clear();
        }
        if !cancel {
            self.print_export_open = open;
        }
    }

    fn open_setup_dialog(&mut self) {
        if let Some(game) = self.current.as_ref() {
            let node = self.node.min(game.nodes.len().saturating_sub(1));
            let pos = &game.nodes[node].pos;
            self.setup_state = crate::setup::PositionSetupState::from_chess(pos);
        } else {
            self.setup_state = crate::setup::PositionSetupState::default();
        }
        self.setup_open = true;
    }

    fn setup_dialog(&mut self, ctx: &egui::Context) {
        if !self.setup_open {
            return;
        }
        let date_str = print_date();
        let action = crate::setup::draw_setup_dialog(
            ctx,
            &mut self.setup_open,
            &mut self.setup_state,
            &self.textures,
            &date_str,
        );
        match action {
            crate::setup::SetupAction::StartGame(game) => {
                let id = self.session.insert_game(game, None);
                self.session_dirty = true;
                self.select_slot(id);
                self.status = "Started new game from setup position.".to_string();
            }
            crate::setup::SetupAction::Cancel | crate::setup::SetupAction::None => {}
        }
    }

    pub fn classify_current_game_opening(
        &mut self,
        overwrite: bool,
    ) -> crate::eco::ClassifyOutcome {
        let Some(game) = self.current.as_mut() else {
            self.classify_status = "No game is open.".to_string();
            self.status = self.classify_status.clone();
            return crate::eco::ClassifyOutcome::Unclassified;
        };
        let outcome = crate::eco::apply_classification(game, overwrite);
        let eco = game.header("ECO").unwrap_or("").to_string();
        let opening = game.header("Opening").unwrap_or("").to_string();
        match outcome {
            crate::eco::ClassifyOutcome::Updated => {
                self.dirty = true;
                self.sync_current_slot();
                let msg = format!("Classified current game as {eco} · {opening}.");
                self.classify_status = msg.clone();
                self.status = msg;
            }
            crate::eco::ClassifyOutcome::PreservedExisting => {
                let msg = format!(
                    "Preserved existing opening tags ({eco}{}). Enable overwrite to replace them.",
                    if opening.is_empty() {
                        String::new()
                    } else {
                        format!(" · {opening}")
                    }
                );
                self.classify_status = msg.clone();
                self.status = msg;
            }
            crate::eco::ClassifyOutcome::Unclassified => {
                let msg = "Current game could not be classified from its mainline (unclassified)."
                    .to_string();
                self.classify_status = msg.clone();
                self.status = msg;
            }
        }
        outcome
    }

    pub fn classify_session_openings(&mut self, overwrite: bool) -> crate::eco::ClassifySummary {
        let mut summary = crate::eco::ClassifySummary::default();
        if self.session.is_empty() {
            if self.current.is_some() {
                let outcome = self.classify_current_game_opening(overwrite);
                summary.record(outcome);
            } else {
                self.classify_status = "There are no games in the session to classify.".to_string();
                self.status = self.classify_status.clone();
            }
            return summary;
        }

        let ids: Vec<usize> = self.session.slots().iter().map(|slot| slot.id).collect();
        let current_slot = self.current_slot;
        let mut session_changed = false;
        for id in ids {
            if current_slot == Some(id)
                && let Some(game) = self.current.as_mut()
            {
                let outcome = crate::eco::apply_classification(game, overwrite);
                summary.record(outcome);
                if outcome == crate::eco::ClassifyOutcome::Updated {
                    self.dirty = true;
                    let snapshot = game.clone();
                    self.session.update_headers(id, &snapshot);
                }
                continue;
            }
            let Ok(mut game) = self.session.load(id, &self.edited) else {
                summary.record(crate::eco::ClassifyOutcome::Unclassified);
                continue;
            };
            let outcome = crate::eco::apply_classification(&mut game, overwrite);
            summary.record(outcome);
            if outcome == crate::eco::ClassifyOutcome::Updated {
                self.session.update_headers(id, &game);
                self.edited.insert(id, game);
                session_changed = true;
            }
        }
        if session_changed {
            self.session_dirty = true;
        }

        let msg = format!(
            "Classified {} games: {} updated, {} preserved existing tags, {} unclassified.",
            summary.total, summary.updated, summary.preserved, summary.unclassified
        );
        self.classify_status = msg.clone();
        self.status = msg;
        summary
    }

    fn classify_openings_dialog(&mut self, ctx: &egui::Context) {
        if !self.classify_openings_open {
            return;
        }
        let mut open = self.classify_openings_open;
        let mut classify_current = false;
        let mut classify_all = false;
        let has_current = self.current.is_some();
        let session_len = self.session.len();
        egui::Window::new("Classify openings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label(
                    RichText::new(
                        "Replay each game's mainline against the CC0 lichess-org/chess-openings dataset and write the deepest matching ECO and Opening tags.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.classify_overwrite,
                    "Overwrite existing ECO and Opening tags",
                )
                .on_hover_text(
                    "When unchecked, games that already have ECO or Opening tags keep their imported metadata.",
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_current, egui::Button::new("Classify current game"))
                        .clicked()
                    {
                        classify_current = true;
                    }
                    let all_label = if session_len > 0 {
                        format!("Classify session ({})", games_label(session_len))
                    } else {
                        "Classify session".to_string()
                    };
                    if ui
                        .add_enabled(session_len > 0, egui::Button::new(all_label))
                        .clicked()
                    {
                        classify_all = true;
                    }
                });
                if !self.classify_status.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&self.classify_status).small().color(GOLD));
                }
            });
        self.classify_openings_open = open;
        let overwrite = self.classify_overwrite;
        if classify_current {
            self.classify_current_game_opening(overwrite);
        } else if classify_all {
            self.classify_session_openings(overwrite);
        }
    }

    /// Load a cached position index, or build one in the background.
    fn start_index(&mut self) {
        let Some(library) = self.library.clone() else {
            return;
        };
        if library.len() > FULL_INDEX_MAX_GAMES {
            self.index = None;
            self.index_rx = None;
            self.index_job = Some(library.path().to_path_buf());
            self.index_status = format!(
                "{} games · full position indexing is disabled for databases this large until the external-sort indexer is available",
                games_label(library.len())
            );
            self.status = self.index_status.clone();
            return;
        }
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

    /// Show the manual position-index action when no cached index is loaded.
    /// Multi-million-game databases are refused until the external-sort
    /// indexer lands; medium-large databases ask for confirmation first.
    fn index_build_button(&self, ui: &mut Ui) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        if self.index.is_some() || self.index_rx.is_some() {
            return false;
        }
        let too_large = library.len() > FULL_INDEX_MAX_GAMES;
        let warn = library.len() > AUTO_INDEX_MAX_GAMES;
        let label = if too_large {
            "Full position index unavailable for this database size"
        } else if warn {
            "Build position index…"
        } else {
            "Build position index"
        };
        let response = ui.add_enabled(!too_large, egui::Button::new(label));
        let response = if too_large {
            response.on_hover_text(
                "Full indexing of multi-million-game databases needs the external-sort indexer planned in the backlog. Use a smaller opening-tree or reference database for now.",
            )
        } else if warn {
            response.on_hover_text(
                "This is a large database. Building the full position index can take several minutes and a lot of RAM. You will be asked to confirm.",
            )
        } else {
            response.on_hover_text("Build or load the cached position index for this database")
        };
        response.clicked()
    }

    fn request_index_build(&mut self) {
        let warn = self
            .library
            .as_ref()
            .is_some_and(|library| library.len() > AUTO_INDEX_MAX_GAMES);
        if warn {
            self.confirm_large_index = true;
        } else {
            self.start_index();
        }
    }

    /// Map and index a native opening book in the background. Polyglot and CTG
    /// books are cheap; Arena and BKT books expand their tables once at open
    /// time.
    fn start_book(&mut self, path: PathBuf) {
        if self.book_job.as_ref() == Some(&path) && self.book_rx.is_some() {
            return;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "bin" | "ctg" | "abk" | "bkt") {
            self.book = None;
            self.book_rx = None;
            self.book_job = None;
            self.book_status = format!(
                "{} is not a supported opening book (use .bin, .ctg, .abk, or .bkt).",
                path.display()
            );
            return;
        }
        self.book = None;
        self.book_rx = None;
        self.book_job = Some(path.clone());
        self.book_status = format!("Opening book {}…", path.display());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = Book::open(&path)
                .map(Arc::new)
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(BookMessage::Done(result));
            ctx.request_repaint();
        });
        self.book_rx = Some(rx);
    }

    fn poll_book(&mut self) {
        let Some(rx) = self.book_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(BookMessage::Done(Ok(book))) => {
                self.book_status = format!("{} · {}", book.path().display(), book.summary());
                self.book = Some(book);
                self.book_job = None;
            }
            Ok(BookMessage::Done(Err(err))) => {
                self.book_status = format!("Opening book failed: {err}");
                self.book_job = None;
            }
            Err(TryRecvError::Empty) => {
                self.book_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.book_status = "Opening-book load stopped unexpectedly.".to_string();
                self.book_job = None;
            }
        }
    }

    /// Index a separately configured opening-tree database in the background.
    fn start_tree_index(&mut self, path: PathBuf) {
        if self.tree_index_job.as_ref() == Some(&path) && self.tree_index_rx.is_some() {
            return;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "bin" | "ctg" | "abk" | "bkt") {
            self.book_path = path.display().to_string();
            self.save_display_prefs();
            self.start_book(path);
            return;
        }
        if index_source_too_large(&path) {
            self.tree_index = None;
            self.tree_library = None;
            self.tree_index_rx = None;
            self.tree_index_job = Some(path.clone());
            self.tree_status = format!(
                "{} is too large for automatic opening-tree indexing. Use a smaller tree database; full indexing above {} games is not available yet.",
                path.display(),
                games_label(FULL_INDEX_MAX_GAMES)
            );
            return;
        }
        self.tree_index = None;
        self.tree_library = None;
        self.tree_index_rx = None;
        self.tree_index_job = Some(path.clone());
        self.tree_status = format!("Checking opening-tree index for {}…", path.display());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = (|| -> Result<(Arc<dyn GameLibrary>, PositionIndex), String> {
                let library = open_library(&path).map_err(|err| format!("{err:#}"))?;
                if library.len() > FULL_INDEX_MAX_GAMES {
                    return Err(format!(
                        "opening-tree database has {} games; full indexing is disabled above {} games",
                        library.len(),
                        FULL_INDEX_MAX_GAMES
                    ));
                }
                let index = PositionIndex::build_cached_for(
                    library.clone(),
                    BuildOptions::default(),
                    move |progress| {
                        let _ = progress_tx.send(TreeIndexMessage::Progress(progress));
                        progress_ctx.request_repaint();
                    },
                )
                .map_err(|err| format!("{err:#}"))?;
                Ok((library, index))
            })();
            let _ = tx.send(TreeIndexMessage::Done(result));
            ctx.request_repaint();
        });
        self.tree_index_rx = Some(rx);
    }

    fn poll_tree_index(&mut self) {
        let Some(rx) = self.tree_index_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(TreeIndexMessage::Progress(progress)) => {
                    self.tree_status = format!(
                        "Indexing opening tree… {} games, {} moves ({:.1}s)",
                        progress.games,
                        progress.edges,
                        progress.elapsed.as_secs_f64()
                    );
                }
                Ok(TreeIndexMessage::Done(Ok((library, index)))) => {
                    self.tree_status =
                        format!("{} · {}", library.path().display(), index_summary(&index));
                    self.tree_library = Some(library);
                    self.tree_index = Some(index);
                    self.tree_index_job = None;
                    keep = false;
                    break;
                }
                Ok(TreeIndexMessage::Done(Err(err))) => {
                    self.tree_status = format!("Opening-tree index failed: {err}");
                    self.tree_index_job = None;
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.tree_status = "Opening-tree index build stopped unexpectedly.".to_string();
                    self.tree_index_job = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.tree_index_rx = Some(rx);
        }
    }

    fn start_correspondence(&mut self, path: PathBuf) {
        if self.correspondence_job.as_ref() == Some(&path) && self.correspondence_rx.is_some() {
            return;
        }
        if index_source_too_large(&path) {
            self.correspondence = None;
            self.correspondence_rx = None;
            self.correspondence_job = Some(path.clone());
            self.correspondence_status = format!(
                "{} is too large for automatic correspondence indexing. Use a smaller reference database.",
                path.display()
            );
            return;
        }
        self.correspondence = None;
        self.correspondence_job = Some(path.clone());
        self.correspondence_status =
            format!("Indexing correspondence reference {}…", path.display());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = (|| -> Result<PositionIndex, String> {
                let library = open_library(&path).map_err(|err| format!("{err:#}"))?;
                if library.len() > FULL_INDEX_MAX_GAMES {
                    return Err(format!(
                        "correspondence database has {} games; automatic indexing is disabled above {} games",
                        library.len(),
                        FULL_INDEX_MAX_GAMES
                    ));
                }
                PositionIndex::build_cached_for(library, BuildOptions::default(), move |progress| {
                    let _ = progress_tx.send(CorrespondenceMessage::Progress(progress));
                    progress_ctx.request_repaint();
                })
                .map_err(|err| format!("{err:#}"))
            })();
            let _ = tx.send(CorrespondenceMessage::Done(result));
            ctx.request_repaint();
        });
        self.correspondence_rx = Some(rx);
    }

    fn poll_correspondence(&mut self) {
        let Some(rx) = self.correspondence_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(CorrespondenceMessage::Progress(progress)) => {
                    self.correspondence_status = format!(
                        "Indexing correspondence reference… {} games, {} moves ({:.1}s)",
                        progress.games,
                        progress.edges,
                        progress.elapsed.as_secs_f64()
                    );
                }
                Ok(CorrespondenceMessage::Done(Ok(index))) => {
                    self.correspondence_status = format!(
                        "Correspondence reference ready · {} games · {} positions",
                        games_label(index.game_count()),
                        positions_label(index.position_count())
                    );
                    self.correspondence = Some(index);
                    self.correspondence_job = None;
                    keep = false;
                    break;
                }
                Ok(CorrespondenceMessage::Done(Err(err))) => {
                    self.correspondence_status = format!("Correspondence reference failed: {err}");
                    self.correspondence = None;
                    self.correspondence_job = None;
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.correspondence_status =
                        "Correspondence reference build stopped unexpectedly.".to_string();
                    self.correspondence_job = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.correspondence_rx = Some(rx);
        }
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

    fn remember_database_dir(&mut self, path: &Path) {
        let Some(parent) = path.parent() else {
            return;
        };
        let parent = if parent.as_os_str().is_empty() {
            std::env::current_dir().ok()
        } else {
            Some(parent.to_path_buf())
        };
        let Some(parent) = parent else {
            return;
        };
        if self.last_database_dir.as_ref() != Some(&parent) {
            self.last_database_dir = Some(parent);
            self.save_display_prefs();
        }
    }

    fn remember_print_dir(&mut self, path: &Path) {
        let Some(parent) = path.parent() else {
            return;
        };
        let parent = if parent.as_os_str().is_empty() {
            std::env::current_dir().ok()
        } else {
            Some(parent.to_path_buf())
        };
        let Some(parent) = parent else {
            return;
        };
        if self.last_print_dir.as_ref() != Some(&parent) {
            self.last_print_dir = Some(parent);
            self.save_display_prefs();
        }
    }

    fn save_display_prefs(&mut self) {
        let prefs = DisplayPrefs {
            rating_priority: self.rating_priority,
            show_all_ratings: self.show_all_ratings,
            engine_view: self.engine_view.config_value().to_string(),
            engine_auto: self.engine_auto,
            opening_tree_path: self.tree_path.clone(),
            opening_book_path: self.book_path.clone(),
            last_database_dir: self.last_database_dir.clone(),
            last_print_dir: self.last_print_dir.clone(),
            prep_recent_limit: self.prep_recent_limit.clamp(1, 50),
            show_empty_pgn_tags: self.show_empty_pgn_tags,
            hover_moves: self.hover_moves,
            show_player_photos: self.show_player_photos,
            photo_size: self.photo_size,
            champion_lineage: self.champion_lineage,
            show_nav_buttons: self.show_nav_buttons,
            show_pgn_tags: self.show_pgn_tags,
            show_annotations: self.show_annotations,
            show_clocks: self.show_clocks,
            engine_bar: self.engine_bar,
            review_skip_book: self.review_skip_book,
            review_book_min_games: self.review_book_min_games.clamp(1, 100),
            review_book_max_plies: self.review_book_max_plies.clamp(4, 80),
            trend_window: self.trend_window,
            trend_baseline: self.trend_baseline,
            trend_min_games: self.trend_min_games.clamp(3, 200),
            engine_limit_mode: self.engine_limit_mode.config_value().to_string(),
            engine_depth: self.engine_depth,
            engine_time_ms: self.engine_time_ms,
        };
        if let Err(err) = prefs.save() {
            self.status = format!("Could not save display settings: {err:#}");
        }
    }

    fn trend_config(&self) -> crate::recent::TrendConfig {
        crate::recent::TrendConfig {
            window: self.trend_window,
            baseline: self.trend_baseline,
            min_recent_games: self.trend_min_games.clamp(3, 200),
        }
    }

    fn photo_texture(&mut self, ctx: &egui::Context, name: &str) -> Option<egui::TextureHandle> {
        const PHOTO_CACHE_LIMIT: usize = 12;
        if !self.show_player_photos {
            return None;
        }
        let key = crate::players::player_key(name)?;
        if let Some(texture) = self.photo_textures.get(&key) {
            return Some(texture.clone());
        }
        let image = {
            let photos = self.photos.as_ref()?;
            let bytes = photos.bytes(name)?;
            crate::photos::decode_webp(bytes).ok()?
        };
        let texture = ctx.load_texture(
            format!("player-photo:{key}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        while self.photo_order.len() >= PHOTO_CACHE_LIMIT {
            if let Some(oldest) = self.photo_order.pop_front() {
                self.photo_textures.remove(&oldest);
            }
        }
        self.photo_order.push_back(key.clone());
        self.photo_textures.insert(key, texture.clone());
        Some(texture)
    }

    fn save_reference_prefs(&mut self) {
        let prefs = PreparationPrefs {
            reference_path: {
                let path = self.reference_path.trim();
                (!path.is_empty()).then(|| PathBuf::from(path))
            },
            correspondence_path: {
                let path = self.correspondence_path.trim();
                (!path.is_empty()).then(|| PathBuf::from(path))
            },
            correspondence_enabled: self.correspondence_enabled,
            reference_enabled: self.reference_enabled,
            rating_band: self.reference_band.clamp(25, 1_000),
            rating_offset: self.reference_offset.clamp(-1_000, 1_000),
            prep_player_names: self.prep_player_names.clone(),
        };
        if let Err(err) = prefs.save() {
            self.reference_status = format!("Could not save reference settings: {err:#}");
        }
    }

    fn open_settings_window(&mut self) {
        self.show_settings = true;
        self.settings_rating_priority = self.rating_priority;
        self.settings_show_all_ratings = self.show_all_ratings;
        self.settings_photo_size = self.photo_size;
        self.settings_champion_lineage = self.champion_lineage;
        self.settings_show_clocks = self.show_clocks;
        self.settings_show_annotations = self.show_annotations;
        self.settings_show_pgn_tags = self.show_pgn_tags;
        self.settings_show_nav_buttons = self.show_nav_buttons;
        self.settings_show_player_photos = self.show_player_photos;
        self.settings_show_empty_pgn_tags = self.show_empty_pgn_tags;
        self.settings_online_reference = self.online_reference_open;
        self.settings_engine_bar = self.engine_bar;
        self.settings_wiki_enabled = self.wiki_enabled;
        self.settings_my_player_names = self.prep_player_names.join("\n");
        self.settings_correspondence_enabled = self.correspondence_enabled;
        self.settings_correspondence_path = if self.correspondence_path.trim().is_empty() {
            default_correspondence_path()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        } else {
            self.correspondence_path.clone()
        };
        self.settings_review_skip_book = self.review_skip_book;
        self.settings_review_book_min_games = self.review_book_min_games;
        self.settings_review_book_max_plies = self.review_book_max_plies;
        self.settings_trend_window = self.trend_window;
        self.settings_trend_baseline = self.trend_baseline;
        self.settings_trend_min_games = self.trend_min_games;
        self.settings_token_from_env = std::env::var("BRETT_LICHESS_TOKEN")
            .ok()
            .is_some_and(|token| !token.trim().is_empty());
        self.settings_token = if self.settings_token_from_env {
            String::new()
        } else {
            self.lichess_token.clone().unwrap_or_default()
        };
        self.settings_token_visible = false;
        self.settings_status.clear();
    }

    /// Apply the settings dialog. Returns `true` when everything was saved.
    fn apply_settings(&mut self) -> bool {
        let mut saved = true;

        if !self.settings_token_from_env {
            let token = self.settings_token.trim();
            let token = (!token.is_empty()).then_some(token);
            match CloudPrefs::save_token(token) {
                Ok(()) => {
                    self.lichess_token = CloudPrefs::load().lichess_token;
                }
                Err(err) => {
                    self.settings_status = format!("Could not save the Lichess token: {err:#}");
                    saved = false;
                }
            }
        }

        let wiki_changed = self.wiki_enabled != self.settings_wiki_enabled;
        self.wiki_enabled = self.settings_wiki_enabled;
        if let Err(err) = CloudPrefs::save_wikipedia_summaries(self.wiki_enabled) {
            self.settings_status = format!("Could not save the Wikipedia setting: {err:#}");
            saved = false;
        }

        self.rating_priority = self.settings_rating_priority;
        self.show_all_ratings = self.settings_show_all_ratings;
        self.photo_size = self.settings_photo_size;
        self.champion_lineage = self.settings_champion_lineage;
        self.show_clocks = self.settings_show_clocks;
        self.show_annotations = self.settings_show_annotations;
        self.show_pgn_tags = self.settings_show_pgn_tags;
        self.show_nav_buttons = self.settings_show_nav_buttons;
        self.show_player_photos = self.settings_show_player_photos;
        self.show_empty_pgn_tags = self.settings_show_empty_pgn_tags;
        self.online_reference_open = self.settings_online_reference;
        self.engine_bar = self.settings_engine_bar;
        self.review_skip_book = self.settings_review_skip_book;
        self.review_book_min_games = self.settings_review_book_min_games.clamp(1, 100);
        self.review_book_max_plies = self.settings_review_book_max_plies.clamp(4, 80);
        self.trend_window = self.settings_trend_window;
        self.trend_baseline = self.settings_trend_baseline;
        self.trend_min_games = self.settings_trend_min_games.clamp(3, 200);
        self.prep_player_names = self
            .settings_my_player_names
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .take(100)
            .map(str::to_string)
            .collect();

        let old_enabled = self.correspondence_enabled;
        let old_path = self.correspondence_path.clone();
        let mut path = self.settings_correspondence_path.trim().to_string();
        if self.settings_correspondence_enabled && path.is_empty() {
            path = default_correspondence_path()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
        }
        let path_changed = path != old_path;
        let enabled_changed = self.settings_correspondence_enabled != old_enabled;
        self.correspondence_enabled = self.settings_correspondence_enabled;
        self.correspondence_path = path.clone();

        if !self.correspondence_enabled {
            self.correspondence = None;
            self.correspondence_rx = None;
            self.correspondence_job = None;
            self.correspondence_status =
                "Correspondence database disabled in Settings.".to_string();
        } else if path.is_empty() {
            self.correspondence = None;
            self.correspondence_rx = None;
            self.correspondence_job = None;
            self.correspondence_status = "No correspondence database configured.".to_string();
        } else {
            let path = PathBuf::from(path);
            if !path.exists() {
                self.correspondence = None;
                self.correspondence_rx = None;
                self.correspondence_job = None;
                self.correspondence_status =
                    format!("Correspondence database not found: {}", path.display());
            } else if path_changed
                || enabled_changed
                || (self.correspondence.is_none()
                    && self.correspondence_rx.is_none()
                    && self.correspondence_job.is_none())
            {
                self.start_correspondence(path);
            }
        }

        self.save_engine_prefs();
        self.save_display_prefs();
        self.save_reference_prefs();
        if wiki_changed {
            if self.wiki_enabled {
                if let Some(index) = self.selected_player
                    && let Some(player) = self.players.get(index).cloned()
                {
                    self.open_wiki_for(&player);
                }
            } else {
                self.reset_wiki();
            }
        }
        self.status = if saved {
            "Settings saved.".to_string()
        } else {
            "Some settings could not be saved.".to_string()
        };
        saved
    }

    fn apply_imported_settings(&mut self, bundle: &crate::settings::SettingsBundle) {
        for (i, slot) in self.engine_slots.iter_mut().enumerate() {
            slot.path = bundle.engine.engines[i]
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            slot.threads = bundle.engine.engines[i].threads;
            slot.hash_mb = bundle.engine.engines[i].hash_mb;
            slot.enabled = bundle.engine.engines[i].enabled;
        }
        self.engine_path = bundle.engine.resolved_path().display().to_string();
        self.engine_threads = bundle.engine.threads;
        self.engine_hash_mb = bundle.engine.hash_mb;
        self.cache_engine_analysis = bundle.engine.cache_engine_analysis;
        self.heuristic_supersede = bundle.engine.heuristic_supersede;
        if let Some(cache) = &self.analysis_cache {
            cache.set_heuristic_supersede(self.heuristic_supersede);
        }
        self.apply_engine_settings();

        self.lichess_token = bundle.references.lichess_token.clone();
        self.explorer_source = bundle.references.explorer_source;
        self.wiki_enabled = bundle.references.wikipedia_summaries;

        self.prep_player_names = bundle.preparation.prep_player_names.clone();
        self.reference_enabled = bundle.preparation.reference_enabled;
        self.reference_band = bundle.preparation.rating_band;
        self.reference_offset = bundle.preparation.rating_offset;
        self.correspondence_enabled = bundle.preparation.correspondence_enabled;
        self.correspondence_path = bundle
            .preparation
            .correspondence_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if let Some(ref_path) = &bundle.preparation.reference_path {
            self.reference_path = ref_path.display().to_string();
        }

        self.rating_priority = bundle.display.rating_priority;
        self.show_all_ratings = bundle.display.show_all_ratings;
        self.photo_size = bundle.display.photo_size;
        self.champion_lineage = bundle.display.champion_lineage;
        self.show_clocks = bundle.display.show_clocks;
        self.show_annotations = bundle.display.show_annotations;
        self.show_pgn_tags = bundle.display.show_pgn_tags;
        self.show_nav_buttons = bundle.display.show_nav_buttons;
        self.show_player_photos = bundle.display.show_player_photos;
        self.show_empty_pgn_tags = bundle.display.show_empty_pgn_tags;
        self.engine_bar = bundle.display.engine_bar;
        self.engine_auto = bundle.display.engine_auto;
        self.engine_view = EngineView::from_config(&bundle.display.engine_view);
        self.review_skip_book = bundle.display.review_skip_book;
        self.review_book_min_games = bundle.display.review_book_min_games;
        self.review_book_max_plies = bundle.display.review_book_max_plies;
        self.trend_window = bundle.display.trend_window;
        self.trend_baseline = bundle.display.trend_baseline;
        self.trend_min_games = bundle.display.trend_min_games;
        self.engine_limit_mode = EngineLimitMode::from_config(&bundle.display.engine_limit_mode);
        self.engine_depth = bundle.display.engine_depth;
        self.engine_time_ms = bundle.display.engine_time_ms;

        self.settings_rating_priority = self.rating_priority;
        self.settings_show_all_ratings = self.show_all_ratings;
        self.settings_photo_size = self.photo_size;
        self.settings_champion_lineage = self.champion_lineage;
        self.settings_show_clocks = self.show_clocks;
        self.settings_show_annotations = self.show_annotations;
        self.settings_show_pgn_tags = self.show_pgn_tags;
        self.settings_show_nav_buttons = self.show_nav_buttons;
        self.settings_show_player_photos = self.show_player_photos;
        self.settings_show_empty_pgn_tags = self.show_empty_pgn_tags;
        self.settings_online_reference = self.online_reference_open;
        self.settings_engine_bar = self.engine_bar;
        self.settings_wiki_enabled = self.wiki_enabled;
        self.settings_my_player_names = self.prep_player_names.join("\n");
        self.settings_correspondence_enabled = self.correspondence_enabled;
        self.settings_correspondence_path = self.correspondence_path.clone();
        self.settings_review_skip_book = self.review_skip_book;
        self.settings_review_book_min_games = self.review_book_min_games;
        self.settings_review_book_max_plies = self.review_book_max_plies;
        self.settings_trend_window = self.trend_window;
        self.settings_trend_baseline = self.trend_baseline;
        self.settings_trend_min_games = self.trend_min_games;
        self.settings_token = self.lichess_token.clone().unwrap_or_default();

        self.status = "Settings successfully imported.".to_string();
    }

    /// Mark the currently open database as the default preparation reference.
    fn set_current_database_as_reference(&mut self) {
        let Some(library) = &self.library else {
            self.reference_status = "Open a database before choosing a reference.".to_string();
            return;
        };
        let path = library.path().display().to_string();
        self.reference_path = path.clone();
        self.reference_enabled = true;
        self.reference = None;
        self.reference_rx = None;
        self.reference_job = None;
        self.reference_status = format!(
            "{path} is now the default reference database. Open Players → Prepare to index it."
        );
        self.save_reference_prefs();
        self.status = format!("Default reference database: {path}");
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
        if index_source_too_large(&job.path) {
            self.reference = None;
            self.reference_rx = None;
            self.reference_job = None;
            self.reference_status = format!(
                "{} is too large for automatic reference indexing. Use a smaller reference database; full indexing above {} games is not available yet.",
                job.path.display(),
                games_label(FULL_INDEX_MAX_GAMES)
            );
            return;
        }
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
                    if library.len() > FULL_INDEX_MAX_GAMES {
                        return Err(anyhow::anyhow!(
                            "reference database has {} games; full reference indexing is disabled above {} games",
                            library.len(),
                            FULL_INDEX_MAX_GAMES
                        ));
                    }
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
                            ..BuildOptions::default()
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

    fn current_position(&self) -> Option<Chess> {
        let game = self.current.as_ref()?;
        let node = game.nodes.get(self.node)?;
        Some(node.pos.clone())
    }

    fn update_cached_analyses(&mut self) {
        let Some(pos) = self.current_position() else {
            self.cached_analyses.clear();
            self.cached_analyses_pos = None;
            return;
        };
        let hash = pos.position_hash();
        if self.cached_analyses_pos == Some(hash) {
            return;
        }
        self.cached_analyses_pos = Some(hash);
        if let Some(cache) = &self.analysis_cache {
            self.cached_analyses = cache.query_position(hash);
        }
    }

    fn record_cache_written(
        &mut self,
        pos_hash: u64,
        batch: &[crate::analysis_cache::CachedAnalysis],
    ) {
        let max_depth = batch.iter().map(|item| item.depth).max().unwrap_or(0);
        self.engine_cache_saved_notice = Some((pos_hash, max_depth, Instant::now()));

        if self.current_position().map(|p| p.position_hash()) == Some(pos_hash) {
            for incoming in batch {
                if let Some(existing) = self.cached_analyses.iter_mut().find(|a| {
                    a.pos_hash == incoming.pos_hash
                        && a.engine_family == incoming.engine_family
                        && a.multipv == incoming.multipv
                }) {
                    if incoming.depth >= existing.depth {
                        *existing = incoming.clone();
                    }
                } else {
                    self.cached_analyses.push(incoming.clone());
                }
            }
            self.cached_analyses
                .sort_by_key(|a| (a.engine_family, a.multipv));
            self.cached_analyses_pos = Some(pos_hash);
        }
    }

    fn engine_limit(&self) -> SearchLimit {
        match self.engine_limit_mode {
            EngineLimitMode::Infinite => SearchLimit::Infinite,
            EngineLimitMode::Depth => SearchLimit::Depth(self.engine_depth),
            EngineLimitMode::Time => {
                SearchLimit::MoveTime(Duration::from_millis(self.engine_time_ms))
            }
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
        let slot = &self.engine_slots[self.selected_engine_slot];
        slot.engine.as_ref().is_some_and(|engine| {
            let path = if slot.path.trim().is_empty() {
                if self.selected_engine_slot == 0 {
                    crate::engine::default_engine_path()
                } else {
                    PathBuf::new()
                }
            } else {
                PathBuf::from(slot.path.trim())
            };
            let options = EngineOptions {
                threads: slot.threads.clamp(1, 256),
                hash_mb: slot.hash_mb.clamp(1, 32_768),
            };
            engine.path() != path || engine.options() != options
        })
    }

    fn ensure_engine_slot(&mut self, idx: usize) -> bool {
        if idx >= 3 || self.engine_view == EngineView::Cloud {
            return false;
        }
        let path_str = self.engine_slots[idx].path.trim();
        let path = if path_str.is_empty() {
            if idx == 0 {
                crate::engine::default_engine_path()
            } else {
                return false;
            }
        } else {
            PathBuf::from(path_str)
        };
        let options = EngineOptions {
            threads: self.engine_slots[idx].threads.clamp(1, 256),
            hash_mb: self.engine_slots[idx].hash_mb.clamp(1, 32_768),
        };
        let needs_start = match &self.engine_slots[idx].engine {
            Some(e) => e.path() != path || e.options() != options,
            None => true,
        };
        if needs_start {
            if let Some(e) = self.engine_slots[idx].engine.take() {
                e.stop();
            }
            match EngineHandle::spawn_with_options(&path, options) {
                Ok(engine) => {
                    self.engine_slots[idx].engine = Some(engine);
                    self.engine_slots[idx].ready = false;
                    self.engine_slots[idx].name.clear();
                    self.engine_slots[idx].status = format!("Starting Engine {}…", idx + 1);
                    if idx == 0 {
                        self.engine_ready = false;
                        self.engine_status = self.engine_slots[0].status.clone();
                    }
                    true
                }
                Err(err) => {
                    self.engine_slots[idx].status = format!("Engine {} error: {err:#}", idx + 1);
                    if idx == 0 {
                        self.engine_status = self.engine_slots[0].status.clone();
                    }
                    false
                }
            }
        } else {
            true
        }
    }

    fn ensure_engine(&mut self) -> bool {
        if self.engine_view == EngineView::Cloud {
            self.engine_status = "Cloud-only mode: local engine is off.".to_string();
            return false;
        }
        let mut any_ok = false;
        for i in 0..3 {
            if self.engine_slots[i].enabled && self.ensure_engine_slot(i) {
                any_ok = true;
            }
        }
        any_ok
    }

    fn toggle_engine_slot(&mut self, idx: usize) {
        if idx >= 3 {
            return;
        }
        self.engine_slots[idx].enabled = !self.engine_slots[idx].enabled;
        let is_enabled = self.engine_slots[idx].enabled;
        let name = self.engine_slots[idx].short_name(idx);
        if is_enabled {
            self.status = format!("Engine {} ({name}) [Hotkey {}] turned ON", idx + 1, idx + 1);
            if self.engine_view != EngineView::Cloud {
                if !self.ensure_engine_slot(idx) {
                    self.status = format!("Engine {} ({name}) failed to start", idx + 1);
                } else if let Some(fen) = self.current_fen() {
                    let multipv = self.engine_multipv;
                    let limit = self.engine_limit();
                    if let Some(engine) = self.engine_slots[idx].engine.as_mut() {
                        let request = engine.analyze(fen.clone(), multipv, limit);
                        self.engine_slots[idx].request = request;
                        self.engine_slots[idx].fen = Some(fen);
                        self.engine_slots[idx].infos.clear();
                        self.engine_slots[idx].bestmove = None;
                        self.engine_slots[idx].searching = true;
                    }
                }
            }
        } else {
            self.status = format!("Engine {} ({name}) [Hotkey {}] turned OFF", idx + 1, idx + 1);
            self.stop_engine_slot(idx);
        }
        self.engine_searching = self.engine_slots.iter().any(|s| s.searching);
        self.save_engine_prefs();
    }

    fn save_engine_prefs(&mut self) {
        let engines = [
            crate::settings::EngineSlotConfig {
                path: if self.engine_slots[0].path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(self.engine_slots[0].path.trim()))
                },
                threads: self.engine_slots[0].threads,
                hash_mb: self.engine_slots[0].hash_mb,
                enabled: self.engine_slots[0].enabled,
            },
            crate::settings::EngineSlotConfig {
                path: if self.engine_slots[1].path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(self.engine_slots[1].path.trim()))
                },
                threads: self.engine_slots[1].threads,
                hash_mb: self.engine_slots[1].hash_mb,
                enabled: self.engine_slots[1].enabled,
            },
            crate::settings::EngineSlotConfig {
                path: if self.engine_slots[2].path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(self.engine_slots[2].path.trim()))
                },
                threads: self.engine_slots[2].threads,
                hash_mb: self.engine_slots[2].hash_mb,
                enabled: self.engine_slots[2].enabled,
            },
        ];
        let prefs = crate::settings::EnginePrefs {
            path: engines[0].path.clone(),
            threads: engines[0].threads,
            hash_mb: engines[0].hash_mb,
            cache_engine_analysis: self.cache_engine_analysis,
            heuristic_supersede: self.heuristic_supersede,
            engines,
        };
        if let Err(err) = prefs.save() {
            self.status = format!("Could not save engine settings: {err:#}");
        }
    }

    fn flush_engine_slot_cache(&mut self, idx: usize) {
        if idx >= 3 {
            return;
        }
        if let Some(batch) = self.engine_slots[idx].debouncer.flush()
            && let Some(cache) = &self.analysis_cache
            && let Some(first) = batch.first()
        {
            let pos_hash = first.pos_hash;
            cache.store_batch(batch.clone());
            self.record_cache_written(pos_hash, &batch);
        }
    }

    fn flush_all_engine_caches(&mut self) {
        for i in 0..3 {
            self.flush_engine_slot_cache(i);
        }
    }

    fn stop_engine_slot(&mut self, idx: usize) {
        if idx >= 3 {
            return;
        }
        if let Some(engine) = &self.engine_slots[idx].engine {
            engine.stop();
        }
        if self.cache_engine_analysis {
            self.flush_engine_slot_cache(idx);
        }
        self.engine_slots[idx].searching = false;
        self.engine_slots[idx].status = "Stopped.".to_string();
        if idx == 0 {
            self.engine_status = "Stopped.".to_string();
        }
        self.engine_searching = self.engine_slots.iter().any(|s| s.searching);
    }

    fn stop_all_engines(&mut self) {
        for i in 0..3 {
            self.stop_engine_slot(i);
        }
        self.engine_searching = false;
        self.engine_status = "Stopped.".to_string();
    }

    fn reset_engine_events(&mut self) {
        if self.cache_engine_analysis {
            self.flush_all_engine_caches();
        }
        for i in 0..3 {
            self.engine_slots[i].request = 0;
            self.engine_slots[i].fen = None;
            self.engine_slots[i].infos.clear();
            self.engine_slots[i].bestmove = None;
            self.engine_slots[i].searching = false;
            self.engine_slots[i].ready = false;
        }
        self.engine_request = 0;
        self.engine_fen = None;
        self.engine_infos.clear();
        self.engine_bestmove = None;
        self.engine_searching = false;
        self.engine_ready = false;
    }

    fn clear_engine_results(&mut self) {
        if self.cache_engine_analysis {
            self.flush_all_engine_caches();
        }
        self.stop_all_engines();
        for i in 0..3 {
            self.engine_slots[i].request = 0;
            self.engine_slots[i].fen = None;
            self.engine_slots[i].infos.clear();
            self.engine_slots[i].bestmove = None;
            self.engine_slots[i].searching = false;
        }
        self.engine_request = 0;
        self.engine_fen = None;
        self.engine_infos.clear();
        self.engine_bestmove = None;
        self.engine_searching = false;
    }

    /// Stop and drop all local engine processes. Used when the Engine tab
    /// switches to Cloud-only mode.
    fn drop_local_engines(&mut self) {
        for i in 0..3 {
            if let Some(engine) = self.engine_slots[i].engine.take() {
                engine.stop();
            }
            if self.cache_engine_analysis {
                self.flush_engine_slot_cache(i);
            }
            self.engine_slots[i].searching = false;
            self.engine_slots[i].ready = false;
            self.engine_slots[i].name.clear();
            self.engine_slots[i].infos.clear();
            self.engine_slots[i].bestmove = None;
            self.engine_slots[i].status = "Off.".to_string();
        }
        if let Some(engine) = self.review_engine.take() {
            engine.stop();
        }
        self.reset_engine_events();
        self.engine_name.clear();
        self.engine_searching = false;
        self.engine_ready = false;
        self.engine_infos.clear();
        self.engine_status = "Cloud-only mode: local engines are off.".to_string();
        if self.review.as_ref().is_some_and(|review| review.running) {
            if let Some(review) = &mut self.review {
                review.cancel();
            }
            self.review_status = "Review stopped: Cloud-only mode.".to_string();
        }
        self.review_open = false;
    }

    /// Save the current engine selection and replace both engine processes.
    fn apply_engine_settings(&mut self) {
        if self.engine_view == EngineView::Cloud {
            self.drop_local_engines();
            return;
        }
        self.stop_all_engines();
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        self.review_engine = None;
        let review_was_running = self.review.as_ref().is_some_and(|review| review.running);
        if review_was_running {
            self.cancel_review();
            self.review_status = "Review canceled: engine settings changed.".to_string();
        }

        self.save_engine_prefs();

        self.analysis_cache = Some(crate::analysis_cache::AnalysisCacheHandle::spawn(
            None,
            self.heuristic_supersede,
        ));
        if self.right_tab_visible(RightTab::Engine) {
            self.ensure_engine();
        } else {
            self.engine_status = "Engine settings saved.".to_string();
        }
    }

    fn request_engine_analysis(&mut self) {
        if self.engine_view == EngineView::Cloud {
            return;
        }
        if !self.ensure_engine() {
            return;
        }
        let Some(fen) = self.current_fen() else {
            return;
        };
        let multipv = self.engine_multipv;
        let limit = self.engine_limit();

        for i in 0..3 {
            if !self.engine_slots[i].enabled {
                continue;
            }
            if let Some(engine) = self.engine_slots[i].engine.as_mut() {
                let request = engine.analyze(fen.clone(), multipv, limit);
                self.engine_slots[i].request = request;
                self.engine_slots[i].fen = Some(fen.clone());
                self.engine_slots[i].infos.clear();
                self.engine_slots[i].bestmove = None;
                self.engine_slots[i].searching = true;
                self.engine_slots[i].status = match limit {
                    SearchLimit::Infinite => "Searching (infinite)…".to_string(),
                    SearchLimit::Depth(depth) => format!("Searching to depth {depth}…"),
                    SearchLimit::MoveTime(time) => {
                        format!("Searching for {} ms…", time.as_millis())
                    }
                };
            }
        }
        self.engine_searching = self.engine_slots.iter().any(|s| s.searching);
        self.engine_infos = self.engine_slots[0].infos.clone();
        self.engine_bestmove = self.engine_slots[0].bestmove.clone();
        self.engine_status = self.engine_slots[0].status.clone();
    }

    fn maybe_engine_analysis(&mut self) {
        let review_busy = self.review.as_ref().is_some_and(|review| review.running)
            || self.review_book_rx.is_some();
        let wants_engine = self.right_tab_visible(RightTab::Engine)
            || (self.engine_bar && self.engine_auto && !review_busy);
        if !wants_engine || self.engine_view == EngineView::Cloud {
            return;
        }
        let any_running = self.engine_slots.iter().any(|s| s.engine.is_some());
        if !any_running {
            if self.engine_status.starts_with("Engine unavailable") {
                return;
            }
            self.ensure_engine();
            return;
        }
        let any_ready = self.engine_slots.iter().any(|s| s.enabled && s.ready);
        if !any_ready || !self.engine_auto {
            return;
        }
        let Some(fen) = self.current_fen() else {
            return;
        };
        let needs_search = self
            .engine_slots
            .iter()
            .any(|s| s.enabled && s.fen.as_deref() != Some(fen.as_str()));
        if needs_search {
            self.request_engine_analysis();
        }
    }

    fn poll_engine(&mut self) {
        if self.engine_view == EngineView::Cloud {
            if self.engine_slots.iter().any(|s| s.engine.is_some()) || self.review_engine.is_some() {
                self.drop_local_engines();
            }
            return;
        }

        for idx in 0..3 {
            let Some(engine) = self.engine_slots[idx].engine.as_ref() else {
                continue;
            };
            let mut events = Vec::new();
            while let Some(event) = engine.try_recv() {
                events.push(event);
            }
            for event in events {
                match event {
                    EngineEvent::Name(name) => {
                        self.engine_slots[idx].name = name.clone();
                        if idx == 0 {
                            self.engine_name = name;
                        }
                    }
                    EngineEvent::Ready => {
                        self.engine_slots[idx].ready = true;
                        self.engine_slots[idx].status = if self.engine_slots[idx].name.is_empty() {
                            format!("Engine {} ready.", idx + 1)
                        } else {
                            format!("{} ready.", self.engine_slots[idx].name)
                        };
                        if idx == 0 {
                            self.engine_ready = true;
                            self.engine_status = self.engine_slots[0].status.clone();
                        }
                    }
                    EngineEvent::Info { request, info } => {
                        if request == self.engine_slots[idx].request {
                            self.engine_slots[idx].infos.insert(info.multipv, info.clone());
                            if idx == 0 {
                                self.engine_infos.insert(info.multipv, info.clone());
                            }
                            if self.cache_engine_analysis
                                && !info.pv.is_empty()
                                && info.score.is_some()
                                && info.depth > 0
                                && let Some(pos) = self.current_position()
                            {
                                let pos_hash = pos.position_hash();
                                let family = crate::analysis_cache::parse_engine_identity(
                                    &self.engine_slots[idx].name,
                                )
                                .family;
                                let (score_cp, score_mate) = match info.score {
                                    Some(Score::Cp(cp)) => (Some(cp), None),
                                    Some(Score::Mate(m)) => (None, Some(m)),
                                    None => (None, None),
                                };
                                let identity = crate::analysis_cache::parse_engine_identity(
                                    &self.engine_slots[idx].name,
                                );
                                let quality_score = crate::analysis_cache::compute_quality_rating(
                                    family,
                                    identity.baseline_elo,
                                    info.depth,
                                    info.seldepth,
                                    info.nodes,
                                    score_mate,
                                );
                                let analyzed_at = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let cached = crate::analysis_cache::CachedAnalysis {
                                    pos_hash,
                                    fen: self.engine_slots[idx]
                                        .fen
                                        .clone()
                                        .unwrap_or_default(),
                                    engine_family: family,
                                    engine_name: self.engine_slots[idx].name.clone(),
                                    multipv: info.multipv,
                                    depth: info.depth,
                                    seldepth: info.seldepth,
                                    score_cp,
                                    score_mate,
                                    nodes: info.nodes,
                                    time_ms: info.time_ms,
                                    nps: info.nps,
                                    pv: info.pv.clone(),
                                    quality_score,
                                    analyzed_at,
                                };
                                if let Some(batch) = self.engine_slots[idx]
                                    .debouncer
                                    .push(cached, Instant::now())
                                    && let Some(cache) = &self.analysis_cache
                                {
                                    cache.store_batch(batch.clone());
                                    self.record_cache_written(pos_hash, &batch);
                                }
                            }
                        }
                    }
                    EngineEvent::BestMove { request, best, .. } => {
                        if request == self.engine_slots[idx].request {
                            self.engine_slots[idx].searching = false;
                            self.engine_slots[idx].bestmove = Some(best.clone());
                            self.engine_slots[idx].status = "Analysis complete.".to_string();
                            if idx == 0 {
                                self.engine_searching =
                                    self.engine_slots.iter().any(|s| s.searching);
                                self.engine_bestmove = Some(best);
                                self.engine_status = "Analysis complete.".to_string();
                            }
                            if self.cache_engine_analysis {
                                self.flush_engine_slot_cache(idx);
                            }
                        }
                    }
                    EngineEvent::Error(err) => {
                        self.engine_slots[idx].status =
                            format!("Engine {} error: {err}", idx + 1);
                        if idx == 0 {
                            self.engine_status = self.engine_slots[0].status.clone();
                        }
                    }
                    EngineEvent::Exited => {
                        self.engine_slots[idx].engine = None;
                        self.engine_slots[idx].ready = false;
                        self.engine_slots[idx].searching = false;
                        self.engine_slots[idx].infos.clear();
                        self.engine_slots[idx].bestmove = None;
                        if idx == 0 {
                            self.engine_ready = false;
                            self.engine_searching =
                                self.engine_slots.iter().any(|s| s.searching);
                            self.engine_infos.clear();
                            self.engine_bestmove = None;
                            self.engine_status = "Engine exited.".to_string();
                        }
                    }
                }
            }
        }
        self.engine_searching = self.engine_slots.iter().any(|s| s.searching);
    }

    fn review_limit(&self) -> SearchLimit {
        if self.review_use_time {
            SearchLimit::MoveTime(Duration::from_millis(self.review_time_ms))
        } else {
            SearchLimit::Depth(self.review_depth)
        }
    }

    /// Move a matching completed review to the front of the reuse cache.
    fn cached_review(&mut self, key: &review::ReviewKey) -> Option<ReviewState> {
        let index = self
            .review_cache
            .iter()
            .position(|(cached, _)| cached == key)?;
        let (key, review) = self.review_cache.remove(index);
        self.review_cache.push((key, review.clone()));
        Some(review)
    }

    /// Keep only completed reviews, replacing any older entry for the same key.
    fn store_review(&mut self, key: review::ReviewKey, review: ReviewState) {
        if !review.complete() {
            return;
        }
        self.review_cache.retain(|(cached, _)| cached != &key);
        if self.review_cache.len() >= REVIEW_CACHE_LIMIT {
            self.review_cache.remove(0);
        }
        self.review_cache.push((key, review));
    }

    /// Forget the cached result for the review on screen so an explicit
    /// **Restart review** reruns the analysis instead of restoring it.
    fn invalidate_current_review_cache(&mut self) {
        if let Some(key) = self.review.as_ref().and_then(|review| review.key.clone()) {
            self.review_cache.retain(|(cached, _)| cached != &key);
        }
    }

    fn ensure_review_engine(&mut self) -> bool {
        if self.engine_view == EngineView::Cloud {
            self.review_status = "Game review is unavailable in Cloud-only mode.".to_string();
            return false;
        }
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
        if self.engine_view == EngineView::Cloud {
            self.review_status = "Game review is unavailable in Cloud-only mode.".to_string();
            self.review_open = false;
            return;
        }
        let Some(game) = self.current.clone() else {
            self.review_status = "Open a game before starting a review.".to_string();
            return;
        };
        // An explicit restart must rerun the analysis instead of restoring the
        // result it is replacing.
        self.invalidate_current_review_cache();
        let config = ReviewConfig {
            limit: self.review_limit(),
        };
        self.review_book_rx = None;
        if !self.review_skip_book {
            self.begin_review(&game, config, 0);
            return;
        }

        let max_plies = (self.review_book_max_plies.clamp(4, 80) as usize).min(game.mainline_len());
        if max_plies == 0 {
            self.begin_review(&game, config, 0);
            return;
        }

        let min_games = self.review_book_min_games.clamp(1, 100);
        let local_hits = self.local_book_hits(&game, max_plies, min_games);
        let local_prefix = local_hits.iter().take_while(|&&hit| hit).count();
        if local_prefix >= max_plies {
            self.begin_review(&game, config, local_prefix);
            return;
        }

        let token = self.lichess_token.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        self.review = None;
        self.review_open = true;
        self.review_status = "Checking opening book…".to_string();
        std::thread::spawn(move || {
            struct PrecomputedHits {
                hits: Vec<bool>,
                cursor: usize,
            }
            impl BookSource for PrecomputedHits {
                fn is_book_move(
                    &mut self,
                    _pos: &Chess,
                    _mv: Option<Move>,
                    _played_uci: &str,
                ) -> bool {
                    let hit = self.hits.get(self.cursor).copied().unwrap_or(false);
                    self.cursor += 1;
                    hit
                }
            }
            let mut local = PrecomputedHits {
                hits: local_hits,
                cursor: 0,
            };
            let mut masters = review_book::MastersExplorerSource::new(
                min_games,
                token,
                review_book::DEFAULT_MASTERS_BUDGET,
            );
            let mut sources: [&mut dyn BookSource; 2] = [&mut local, &mut masters];
            let book_plies = review_book::detect_prefix(&game, &mut sources, max_plies);
            let _ = tx.send(ReviewBookMessage {
                game,
                config,
                book_plies,
            });
            ctx.request_repaint();
        });
        self.review_book_rx = Some(rx);
    }

    fn local_book_hits(&self, game: &Game, max_plies: usize, min_games: u32) -> Vec<bool> {
        let mut hits = Vec::with_capacity(max_plies);
        let tree_index = self.tree_index.as_ref().or(self.index.as_ref());
        let mut cur = 0usize;
        for _ in 0..max_plies {
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
            let mut in_book = false;
            if let Some(book) = self.book.as_deref() {
                let mut src = review_book::NativeBookSource::new(book);
                if src.is_book_move(pos, mv, &ply.uci) {
                    in_book = true;
                }
            }
            if !in_book && let Some(index) = tree_index {
                let mut src = review_book::PositionIndexSource::new(index, min_games);
                if src.is_book_move(pos, mv, &ply.uci) {
                    in_book = true;
                }
            }
            hits.push(in_book);
            cur = next;
        }
        hits
    }

    fn begin_review(&mut self, game: &Game, config: ReviewConfig, book_plies: usize) {
        let key = review::ReviewKey::new(
            game,
            config,
            book_plies,
            self.engine_path(),
            self.engine_options(),
        );
        if let Some(cached) = self.cached_review(&key) {
            let total = cached.total();
            let skipped = cached.book_plies;
            self.review = Some(cached);
            self.review_open = true;
            self.review_status = if skipped > 0 {
                format!(
                    "Review restored from cache: {total} positions ({skipped} book plies skipped)."
                )
            } else {
                format!("Review restored from cache: {total} positions.")
            };
            return;
        }

        let mut review = ReviewState::new(game, config, book_plies);
        review.key = Some(key);
        if review.total() == 0 {
            self.review = Some(review);
            self.review_open = true;
            self.review_status = if book_plies > 0 {
                format!("Review complete: all {book_plies} plies are in the opening book.")
            } else {
                "This game has no positions to review.".to_string()
            };
            return;
        }
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
        self.review_open = true;
        self.review_status = if book_plies > 0 {
            format!("Reviewing {total} positions ({book_plies} book plies skipped)…")
        } else {
            format!("Reviewing {total} positions…")
        };
    }

    fn poll_review_book(&mut self) {
        let Some(rx) = self.review_book_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(msg) => {
                let same_game = self.current.as_ref().is_some_and(|cur| {
                    cur.mainline_len() == msg.game.mainline_len()
                        && cur.white() == msg.game.white()
                        && cur.black() == msg.game.black()
                });
                if same_game {
                    self.begin_review(&msg.game, msg.config, msg.book_plies);
                }
            }
            Err(TryRecvError::Empty) => {
                self.review_book_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                if let Some(game) = self.current.clone() {
                    let config = ReviewConfig {
                        limit: self.review_limit(),
                    };
                    self.begin_review(&game, config, 0);
                }
            }
        }
    }

    fn cancel_review(&mut self) {
        self.review_book_rx = None;
        if let Some(engine) = &self.review_engine {
            engine.stop();
        }
        if let Some(review) = &mut self.review {
            review.cancel();
        }
        self.review_status = "Review stopped.".to_string();
    }

    fn clear_review(&mut self) {
        self.review_book_rx = None;
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
                    let mut completed = None;
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
                        if review.complete()
                            && let Some(key) = review.key.clone()
                        {
                            completed = Some((key, review.clone()));
                        }
                    }
                    if let Some(status) = status {
                        self.review_status = status;
                    }
                    if let Some((key, review)) = completed {
                        self.store_review(key, review);
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
        self.wiki_expanded = false;
        self.start_player_book(index);
        self.open_wiki_for(&player);
    }

    /// Open or create the profile for the named player in the Players tab.
    fn open_player_profile(&mut self, name: &str) {
        let index = self.players.ensure_player(name);
        self.select_player(index);
    }

    /// Load the Wikipedia card for a profile. This is the only entry point
    /// that may start a Wikipedia request: database installation and player
    /// directory builds never call it.
    fn open_wiki_for(&mut self, player: &PlayerSummary) {
        if !self.wiki_enabled {
            self.reset_wiki();
            return;
        }
        if self.wiki_for.as_deref() == Some(player.key.as_str()) {
            match &self.wiki_state {
                WikiState::Loading | WikiState::Ready(_) => return,
                WikiState::Idle | WikiState::Failed(_) => {}
            }
        }
        if let Some(lookup) = self.wiki_cache.get(&player.name) {
            self.wiki_rx = None;
            self.wiki_job = None;
            self.wiki_for = Some(player.key.clone());
            self.wiki_state = WikiState::Ready(lookup);
            return;
        }
        self.start_wiki_lookup(player);
    }

    fn start_wiki_lookup(&mut self, player: &PlayerSummary) {
        if self.wiki_job.as_deref() == Some(player.key.as_str()) && self.wiki_rx.is_some() {
            return;
        }
        let key = player.key.clone();
        let name = player.name.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = crate::wiki::lookup(&name);
            let _ = tx.send(WikiMessage::Done {
                player_key: key,
                result,
            });
            ctx.request_repaint();
        });
        self.wiki_rx = Some(rx);
        self.wiki_job = Some(player.key.clone());
        self.wiki_for = Some(player.key.clone());
        self.wiki_state = WikiState::Loading;
    }

    fn reset_wiki(&mut self) {
        self.wiki_rx = None;
        self.wiki_job = None;
        self.wiki_for = None;
        self.wiki_state = WikiState::Idle;
    }

    fn poll_wiki(&mut self) {
        let Some(rx) = self.wiki_rx.as_ref() else {
            return;
        };
        let message = match rx.try_recv() {
            Ok(message) => message,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.wiki_rx = None;
                self.wiki_job = None;
                if self.wiki_for.is_some() {
                    self.wiki_state =
                        WikiState::Failed("The Wikipedia lookup stopped unexpectedly.".to_string());
                }
                return;
            }
        };
        self.wiki_rx = None;
        self.wiki_job = None;
        let WikiMessage::Done { player_key, result } = message;
        let cache_changed = result.is_ok();
        self.apply_wiki_result(player_key, result);
        if cache_changed && let Err(err) = self.wiki_cache.save() {
            tracing::warn!("could not save the Wikipedia cache: {err:#}");
        }
    }

    fn apply_wiki_result(
        &mut self,
        player_key: String,
        result: Result<crate::wiki::Lookup, String>,
    ) {
        if let Ok(lookup) = &result {
            self.wiki_cache.insert(&player_key, lookup);
        }
        if self.wiki_for.as_deref() == Some(player_key.as_str()) {
            self.wiki_state = match result {
                Ok(lookup) => WikiState::Ready(lookup),
                Err(err) => WikiState::Failed(err),
            };
        }
    }

    /// Short, attributed Wikipedia card shown between the profile statistics
    /// and the preparation buttons.
    fn wiki_summary_section(&mut self, ui: &mut Ui, player: &PlayerSummary) {
        if !self.wiki_enabled {
            ui.label(
                RichText::new("Wikipedia summaries are disabled in Settings.")
                    .small()
                    .weak(),
            );
            return;
        }
        let mut retry = false;
        match &self.wiki_state {
            WikiState::Idle => {}
            WikiState::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Looking up Wikipedia…").small().weak());
                });
            }
            WikiState::Ready(crate::wiki::Lookup::Summary(summary)) => {
                let extract = &summary.extract;
                let is_long = extract.chars().count() > 320;
                if is_long && !self.wiki_expanded {
                    let excerpt = summary.excerpt(320);
                    let trimmed = excerpt.trim_end_matches('…');
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(trimmed).small());
                        if ui
                            .link(RichText::new("… Read more").small().color(GOLD))
                            .on_hover_text("Expand the full article extract")
                            .clicked()
                        {
                            self.wiki_expanded = true;
                        }
                    });
                } else {
                    ui.label(RichText::new(extract).small());
                    if is_long
                        && ui
                            .link(RichText::new("Read less").small().weak())
                            .on_hover_text("Collapse the article extract")
                            .clicked()
                    {
                        self.wiki_expanded = false;
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .link(RichText::new(format!("Wikipedia · {} ↗", summary.title)).small())
                        .on_hover_text("Open the full article in your browser")
                        .clicked()
                    {
                        open_browser_url(ui.ctx(), &summary.url);
                    }
                    ui.label(RichText::new("CC BY-SA 4.0").small().weak())
                        .on_hover_text(
                            "Summary text from Wikipedia, licensed CC BY-SA 4.0. Brett caches it locally and does not bundle Wikipedia content.",
                        );
                });
            }
            WikiState::Ready(crate::wiki::Lookup::Ambiguous { search_url }) => {
                ui.label(
                    RichText::new("No confident chess-player match on Wikipedia.")
                        .small()
                        .weak(),
                );
                if ui
                    .link(RichText::new("Search Wikipedia ↗").small())
                    .on_hover_text("The name may refer to a different person")
                    .clicked()
                {
                    open_browser_url(ui.ctx(), search_url);
                }
            }
            WikiState::Failed(err) => {
                ui.label(
                    RichText::new(format!("Wikipedia lookup failed: {err}"))
                        .small()
                        .color(Color32::from_rgb(0xE0, 0x8A, 0x7A)),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("Retry").clicked() {
                        retry = true;
                    }
                    let search = crate::wiki::search_url(&player.name);
                    if ui
                        .link(RichText::new("Search Wikipedia ↗").small())
                        .on_hover_text("Search Wikipedia for this name in your browser")
                        .clicked()
                    {
                        open_browser_url(ui.ctx(), &search);
                    }
                });
            }
        }
        if retry {
            self.start_wiki_lookup(player);
        }
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
            let previous = self.current_slot;
            if self.dirty
                && let (Some(id), Some(game)) = (self.current_slot, self.current.take())
            {
                self.edited.insert(id, game);
            }
            previous
        };

        let display_name = natural_player_name(&player.name);
        let you_name = prep_you_name(&self.prep_player_names);
        let (white, black) = if as_white {
            (you_name, display_name.clone())
        } else {
            (display_name.clone(), you_name)
        };
        let mut headers = vec![
            ("Event".to_string(), "Preparation".to_string()),
            ("White".to_string(), white),
            ("Black".to_string(), black),
            ("Date".to_string(), pgn_date(players::today())),
            ("Result".to_string(), "*".to_string()),
        ];
        if let Some(title) = player.title {
            headers.push((
                if as_white { "WhiteTitle" } else { "BlackTitle" }.to_string(),
                title.short().to_string(),
            ));
        }
        if let Some(id) = &player.fide_id {
            headers.push((
                if as_white {
                    "WhiteFideId"
                } else {
                    "BlackFideId"
                }
                .to_string(),
                id.clone(),
            ));
        }
        let game = Game {
            headers,
            ..Game::default()
        };
        let previous_orientation = self.orientation;
        self.orientation = my_player_color(&self.prep_player_names, &game).unwrap_or(our_color);

        self.clear_review();
        self.recent_preview = None;
        self.prep_online_games.clear();
        self.prep_online_status.clear();
        self.prep_online_rx = None;
        self.prep_online_job = None;
        self.prep = Some(PrepSession {
            player_key: player.key.clone(),
            player_name: display_name.clone(),
            player_title: player.title,
            player_fide_id: player.fide_id.clone(),
            our_color,
            previous_game,
            previous_orientation,
        });
        self.annotation_editor = None;
        self.annotation_focus = false;
        self.reset_branch_state();
        self.print_diagrams.clear();
        self.current = Some(game);
        self.current_slot = None;
        self.dirty = false;
        self.focus_right_tab(RightTab::Tree);
        self.goto(0);
        self.status = format!(
            "Preparing against {} as {} · board from {}",
            display_name,
            color_name(our_color),
            color_name(self.orientation),
        );
        self.start_player_book(index);
    }

    fn exit_prep(&mut self) {
        let Some(prep) = self.prep.take() else {
            return;
        };
        self.recent_preview = None;
        self.prep_online_games.clear();
        self.prep_online_status.clear();
        self.prep_online_rx = None;
        self.prep_online_job = None;
        self.dirty = false;
        self.current = None;
        self.current_slot = None;
        self.annotation_editor = None;
        self.annotation_focus = false;
        self.orientation = prep.previous_orientation;
        let target = prep.previous_game.or_else(|| self.session.id_at(0));
        if let Some(id) = target {
            self.select_slot(id);
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

    fn poll_online_game(&mut self) {
        let Some(rx) = self.online_game_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(game)) => {
                self.online_game_job = None;
                self.open_game_preview(game, "Lichess top game");
            }
            Ok(Err(err)) => {
                self.online_game_job = None;
                self.status = format!("Could not load the Lichess game: {err}");
            }
            Err(TryRecvError::Empty) => {
                self.online_game_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.online_game_job = None;
                self.status = "Lichess game download stopped unexpectedly.".to_string();
            }
        }
    }

    fn start_lichess_game(&mut self, id: String) {
        if self.online_game_rx.is_some() {
            return;
        }
        self.online_game_job = Some(id.clone());
        self.status = format!("Loading Lichess game {id}…");
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = online::fetch_lichess_game(&id).map_err(|err| format!("{err:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.online_game_rx = Some(rx);
    }

    fn poll_online_reference(&mut self) {
        let Some(rx) = self.online_ref_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(games)) => {
                let source = self
                    .online_ref_job
                    .as_ref()
                    .map(|(source, _)| *source)
                    .unwrap_or(self.online_ref_source);
                let username = self
                    .online_ref_job
                    .as_ref()
                    .map(|(_, username)| username.clone())
                    .unwrap_or_default();
                self.online_ref_status = format!(
                    "Loaded {} from {} for {username}.",
                    games_label(games.len()),
                    source.label()
                );
                self.online_ref_games = games;
                self.online_ref_job = None;
            }
            Ok(Err(err)) => {
                self.online_ref_status = format!("Online reference failed: {err}");
                self.online_ref_job = None;
            }
            Err(TryRecvError::Empty) => {
                self.online_ref_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.online_ref_status =
                    "Online reference download stopped unexpectedly.".to_string();
                self.online_ref_job = None;
            }
        }
    }

    fn start_online_reference(&mut self) {
        if self.online_ref_rx.is_some() {
            return;
        }
        let username = self.online_ref_username.trim().to_string();
        if username.is_empty() {
            self.online_ref_status =
                "Enter a username, or a ChessABC player name or FIDE id, first.".to_string();
            return;
        }
        let source = self.online_ref_source;
        self.online_ref_job = Some((source, username.clone()));
        self.online_ref_status = format!("Downloading {username}'s {} games…", source.label());
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = online::fetch_player_games(source, &username, 100)
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.online_ref_rx = Some(rx);
    }

    fn start_prep_online(&mut self) {
        if self.prep_online_rx.is_some() {
            return;
        }
        let Some(prep) = &self.prep else {
            return;
        };
        let player_name = prep.player_name.clone();
        let limit = self.prep_recent_limit.clamp(1, 50);
        self.prep_online_job = Some(player_name.clone());
        self.prep_online_status = format!("Loading {player_name}'s latest ChessABC games…");
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = online::fetch_player_games(OnlineSource::ChessAbc, &player_name, limit)
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.prep_online_rx = Some(rx);
    }

    fn poll_prep_online(&mut self) {
        let Some(rx) = self.prep_online_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(games)) => {
                let name = self
                    .prep_online_job
                    .take()
                    .or_else(|| self.prep.as_ref().map(|prep| prep.player_name.clone()))
                    .unwrap_or_default();
                self.prep_online_status = format!(
                    "Loaded {} from ChessABC for {name}.",
                    games_label(games.len())
                );
                self.prep_online_games = games;
            }
            Ok(Err(err)) => {
                self.prep_online_job = None;
                self.prep_online_status = format!("ChessABC download failed: {err}");
            }
            Err(TryRecvError::Empty) => {
                self.prep_online_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.prep_online_job = None;
                self.prep_online_status = "ChessABC download stopped unexpectedly.".to_string();
            }
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
                    && snapshot.explorer_source == self.explorer_source
                {
                    self.cloud = Some(snapshot.clone());
                    self.cloud_status = if snapshot.has_any_data() {
                        "References loaded.".to_string()
                    } else {
                        "No external references found for this position.".to_string()
                    };
                }
                self.cloud_job = None;
                self.cloud_cache
                    .insert((snapshot.explorer_source, fen), snapshot);
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

    fn poll_explorer(&mut self) {
        let Some(rx) = self.explorer_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(ExplorerMessage::Done {
                fen,
                source,
                result,
            }) => {
                self.explorer_job = None;
                if self.current_fen().as_deref() == Some(fen.as_str())
                    && source == self.explorer_source
                {
                    match &result {
                        Ok(explorer) => {
                            if let Some(snapshot) = self.cloud.as_mut()
                                && snapshot.fen == fen
                            {
                                snapshot.explorer = Ok(explorer.clone());
                                snapshot.explorer_source = source;
                                self.cloud_status =
                                    "Lichess explorer reloaded; other references kept.".to_string();
                            }
                        }
                        Err(err) => {
                            if let Some(snapshot) = self.cloud.as_mut()
                                && snapshot.fen == fen
                            {
                                snapshot.explorer = Err(err.clone());
                                snapshot.explorer_source = source;
                            }
                            self.cloud_status = format!("Lichess explorer refresh failed: {err}");
                        }
                    }
                    if let Some(snapshot) = self.cloud.clone() {
                        self.cloud_cache.insert((source, fen), snapshot);
                    }
                }
            }
            Err(TryRecvError::Empty) => {
                self.explorer_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.explorer_job = None;
                self.cloud_status = "Lichess explorer refresh stopped unexpectedly.".to_string();
            }
        }
    }

    fn start_explorer_refresh(&mut self, fen: String) {
        if self.explorer_rx.is_some() {
            return;
        }
        let source = self.explorer_source;
        self.explorer_job = Some((fen.clone(), source));
        self.cloud_status = format!("Reloading the {} opening explorer…", source.label());
        let token = self.lichess_token.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = cloud::fetch_explorer_only(&fen, token.as_deref(), source);
            let _ = tx.send(ExplorerMessage::Done {
                fen,
                source,
                result,
            });
            ctx.request_repaint();
        });
        self.explorer_rx = Some(rx);
    }

    /// Query the external references for the current position. Queries are
    /// delayed briefly so holding an arrow key does not fire a request for
    /// every intermediate position.
    fn maybe_request_cloud(&mut self) {
        self.update_cached_analyses();
        let wants_tab = |tab: RightTab| match tab {
            RightTab::References | RightTab::Tree => true,
            RightTab::Engine => self.engine_view != EngineView::Local,
            RightTab::Moves => false,
        };
        let wants_cloud = if self.multi_view {
            RightTab::ALL
                .iter()
                .copied()
                .any(|tab| self.visible_tabs[tab.index()] && wants_tab(tab))
        } else {
            wants_tab(self.right_tab)
        };
        if !wants_cloud {
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
        if self.cloud.as_ref().is_some_and(|snapshot| {
            snapshot.fen == fen && snapshot.explorer_source == self.explorer_source
        }) {
            return;
        }
        let cache_key = (self.explorer_source, fen.clone());
        if let Some(snapshot) = self.cloud_cache.get(&cache_key).cloned() {
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
        let explorer_source = self.explorer_source;
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let snapshot = cloud::fetch(&fen, token.as_deref(), explorer_source);
            let _ = tx.send(CloudMessage::Done(snapshot));
            ctx.request_repaint();
        });
        self.cloud_rx = Some(rx);
    }

    fn refresh_cloud(&mut self) {
        if let Some(fen) = self.current_fen() {
            self.cloud_cache
                .remove(&(self.explorer_source, fen.clone()));
            self.cloud = None;
            self.cloud_pending = None;
            self.cloud_job = None;
            self.cloud_rx = None;
            self.explorer_rx = None;
            self.explorer_job = None;
            self.start_cloud(fen);
        }
    }

    fn poll_bar_cloud(&mut self) {
        let Some(rx) = self.bar_cloud_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(BarCloudMessage { fen, result }) => {
                self.bar_cloud_job = None;
                let cached = result.unwrap_or(None);
                self.bar_cloud_cache.insert(fen, cached);
                if self.bar_cloud_cache.len() > 256
                    && let Some(key) = self.bar_cloud_cache.keys().next().cloned()
                {
                    self.bar_cloud_cache.remove(&key);
                }
            }
            Err(TryRecvError::Empty) => {
                self.bar_cloud_rx = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.bar_cloud_job = None;
            }
        }
    }

    fn maybe_request_bar_cloud(&mut self) {
        self.update_cached_analyses();
        if !self.engine_bar {
            return;
        }
        let Some(fen) = self.current_fen() else {
            return;
        };
        if self.cloud.as_ref().is_some_and(|s| s.fen == fen)
            || self.bar_cloud_cache.contains_key(&fen)
            || self.cloud_cache.keys().any(|(_, f)| f == &fen)
        {
            self.bar_cloud_pending = None;
            return;
        }
        if self.cloud_rx.is_some() && self.cloud_job.as_deref() == Some(fen.as_str()) {
            return;
        }
        if self.bar_cloud_rx.is_some() {
            return;
        }
        let now = Instant::now();
        match self.bar_cloud_pending.take() {
            Some((pending, started)) if pending == fen => {
                if now.duration_since(started) < Duration::from_millis(350) {
                    self.bar_cloud_pending = Some((pending, started));
                    self.ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                self.bar_cloud_job = Some(fen.clone());
                let (tx, rx) = mpsc::channel();
                let ctx = self.ctx.clone();
                std::thread::spawn(move || {
                    let result = cloud::fetch_eval_only(&fen);
                    let _ = tx.send(BarCloudMessage { fen, result });
                    ctx.request_repaint();
                });
                self.bar_cloud_rx = Some(rx);
            }
            _ => {
                self.bar_cloud_pending = Some((fen, now));
                self.ctx.request_repaint_after(Duration::from_millis(350));
            }
        }
    }

    fn current_board_eval(&self) -> Option<BoardEval> {
        let game = self.game()?;
        let node = game.nodes.get(self.node)?;
        let pos = &node.pos;
        let fen = Fen::from_position(pos, EnPassantMode::Legal).to_string();
        let turn = pos.turn();

        let local = if self.engine_fen.as_deref() == Some(fen.as_str()) {
            self.engine_infos
                .get(&1)
                .or_else(|| self.engine_infos.values().next())
                .and_then(|info| {
                    info.score
                        .map(|score| BoardEval::from_local(score, turn, info.depth))
                })
        } else if let Some((rev_fen, rev_turn, info)) = self
            .review
            .as_ref()
            .and_then(ReviewState::current_search_eval)
            && rev_fen == fen
        {
            info.score
                .map(|score| BoardEval::from_local(score, rev_turn, info.depth))
        } else if !self.cached_analyses.is_empty() {
            let sf = self.cached_analyses.iter().find(|a| {
                a.engine_family == crate::analysis_cache::EngineFamily::Stockfish && a.multipv == 1
            });
            let lc0 = self.cached_analyses.iter().find(|a| {
                a.engine_family == crate::analysis_cache::EngineFamily::Leela && a.multipv == 1
            });
            if let Some(sf) = sf {
                let score = if let Some(mate) = sf.score_mate {
                    Score::Mate(mate)
                } else if let Some(cp) = sf.score_cp {
                    Score::Cp(cp)
                } else {
                    Score::Cp(0)
                };
                Some(BoardEval::from_local(score, turn, sf.depth))
            } else if let Some(lc0) = lc0 {
                let score = if let Some(mate) = lc0.score_mate {
                    Score::Mate(mate)
                } else if let Some(cp) = lc0.score_cp {
                    let translated = crate::analysis_cache::leela_to_stockfish_cp(cp);
                    Score::Cp(translated)
                } else {
                    Score::Cp(0)
                };
                Some(BoardEval::from_local(score, turn, lc0.depth))
            } else {
                None
            }
        } else {
            None
        };

        let cloud_eval = self
            .cloud
            .as_ref()
            .filter(|s| s.fen == fen)
            .and_then(|s| s.cloud_eval.as_ref().ok())
            .or_else(|| {
                self.cloud_cache
                    .iter()
                    .find(|((_, f), _)| f == &fen)
                    .and_then(|(_, s)| s.cloud_eval.as_ref().ok())
            })
            .or_else(|| self.bar_cloud_cache.get(&fen).and_then(Option::as_ref));
        let cloud = cloud_eval.and_then(|ce| {
            let pv = ce.pvs.first()?;
            BoardEval::from_cloud(pv.cp, pv.mate, ce.depth)
        });

        let pgn = node.eval.and_then(BoardEval::from_pgn);

        eval::choose(local, cloud, pgn)
    }

    /// Load a game by its original source index. Player lists use this path.
    fn select_source(&mut self, source_index: usize) {
        if let Some(id) = self.session.source_slot(source_index) {
            self.select_slot(id);
        } else {
            self.status = format!("Game {} is no longer in the session.", source_index + 1);
        }
    }

    /// Load one editable row, replacing the current game.
    fn select_slot(&mut self, id: usize) {
        if self.current_slot == Some(id) && self.current.is_some() && self.prep.is_none() {
            return;
        }
        self.print_diagrams.clear();
        self.clear_review();
        self.reset_branch_state();
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
            if let (Some(leaving), Some(game)) = (self.current_slot, self.current.take()) {
                self.edited.insert(leaving, game);
            }
            self.dirty = false;
        }

        let loaded = match self.edited.remove(&id) {
            Some(game) => Ok(game),
            None => self.session.load(id, &self.edited),
        };
        let position = self.session.position_of(id);
        self.current_slot = Some(id);
        self.focus_right_tab(RightTab::Moves);
        self.goto(0);
        match loaded {
            Ok(game) => {
                let label = position
                    .map(|position| format!("game {} of {}", position + 1, self.session.len()))
                    .unwrap_or_else(|| format!("game {id}"));
                self.status = if game.errors.is_empty() {
                    format!("{} · {label}", self.index_status)
                } else {
                    format!(
                        "{} · {label}: {} import warnings",
                        self.index_status,
                        game.errors.len()
                    )
                };
                self.current = Some(game);
                let orientation = self
                    .current
                    .as_ref()
                    .and_then(|game| my_player_color(&self.prep_player_names, game));
                if let Some(color) = orientation {
                    self.orientation = color;
                }
            }
            Err(err) => {
                self.current = None;
                self.status = format!("Could not load game: {err}");
            }
        }
    }

    fn has_unsaved_changes(&self) -> bool {
        self.dirty || self.session_dirty
    }

    /// Intercept a window-close request. Modified databases ask first; a
    /// cancelled save leaves the window open.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.close_confirm_open
            || self.save_database_dialog_open
            || self.pending_pgn_export.is_some()
            || self.pending_scid_export.is_some()
            || self.pending_cbh_export.is_some()
            || self.pending_two_cbh_export.is_some()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }
        if self.has_unsaved_changes() {
            self.close_confirm_open = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else {
            self.pending_window_close = true;
        }
    }

    fn close_confirmation_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_confirm_open {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut discard = false;
        let mut cancel = false;
        let can_save = !self.session.is_empty();
        egui::Window::new("Unsaved changes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label("This database has unsaved changes. Save before closing?");
                ui.label(
                    RichText::new(
                        "Save database keeps the current format. 2CBH writes moves, variations, and metadata; comments, NAGs, clocks, and evaluations are not written yet.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .on_disabled_hover_text("There are no games to save.")
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Discard changes").clicked() {
                        discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if save {
            self.close_confirm_open = false;
            self.save_database_now(true);
        } else if discard {
            self.close_confirm_open = false;
            self.dirty = false;
            self.session_dirty = false;
            self.pending_window_close = true;
        } else if cancel || !open {
            self.close_confirm_open = false;
        }
    }

    /// Copy the live current game into `edited` before a whole-database save.
    /// The dirty flag stays set until the target file is actually written.
    fn persist_current_edit(&mut self) {
        if !self.dirty {
            return;
        }
        if let (Some(id), Some(game)) = (self.current_slot, self.current.as_ref()) {
            self.edited.insert(id, game.clone());
        }
    }

    /// Keep the row's cached headers in sync while the selected game is being
    /// edited. The game tree itself stays in `edited` until navigation.
    fn sync_current_slot(&mut self) {
        if !self.dirty {
            return;
        }
        if let (Some(id), Some(game)) = (self.current_slot, self.current.as_ref()) {
            self.session.update_headers(id, game);
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

    fn handle_branch_keys(&mut self, ctx: &egui::Context) {
        let Some(choice) = self.branch_choice.as_ref() else {
            return;
        };
        let len = choice.options.len();
        if len == 0 {
            self.branch_choice = None;
            return;
        }
        let (up, down, left, right, enter, digits) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Enter),
                [
                    i.key_pressed(Key::Num1),
                    i.key_pressed(Key::Num2),
                    i.key_pressed(Key::Num3),
                    i.key_pressed(Key::Num4),
                    i.key_pressed(Key::Num5),
                    i.key_pressed(Key::Num6),
                    i.key_pressed(Key::Num7),
                    i.key_pressed(Key::Num8),
                    i.key_pressed(Key::Num9),
                ],
            )
        });
        let mut selected = choice.selected.min(len - 1);
        if up {
            selected = (selected + len - 1) % len;
        }
        if down {
            selected = (selected + 1) % len;
        }
        let mut chosen = None;
        for (index, pressed) in digits.iter().enumerate() {
            if *pressed && index < len {
                chosen = Some(index);
            }
        }
        if enter || right {
            chosen = Some(selected);
        }
        if left {
            self.branch_choice = None;
            return;
        }
        if let Some(index) = chosen {
            let node = self
                .branch_choice
                .as_ref()
                .and_then(|choice| choice.options.get(index).copied());
            self.branch_choice = None;
            if let Some(node) = node {
                self.goto(node);
            }
        } else if let Some(choice) = self.branch_choice.as_mut() {
            choice.selected = selected;
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let cancel = ctx.input(|i| i.key_pressed(Key::Escape));
        if cancel {
            if self.branch_choice.take().is_some() {
                return;
            }
            if self.date_picker.is_some() {
                self.date_picker = None;
                self.date_picker_anchor = None;
            }
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
        if self.branch_choice.is_some() {
            self.handle_branch_keys(ctx);
            return;
        }

        let (k1, k2, k3) = ctx.input(|i| {
            let alt_or_ctrl = i.modifiers.alt || i.modifiers.ctrl || i.modifiers.command;
            (
                i.key_pressed(Key::Num1) || (alt_or_ctrl && i.key_pressed(Key::Num1)),
                i.key_pressed(Key::Num2) || (alt_or_ctrl && i.key_pressed(Key::Num2)),
                i.key_pressed(Key::Num3) || (alt_or_ctrl && i.key_pressed(Key::Num3)),
            )
        });
        if k1 {
            self.toggle_engine_slot(0);
        }
        if k2 {
            self.toggle_engine_slot(1);
        }
        if k3 {
            self.toggle_engine_slot(2);
        }
        let (
            right,
            left,
            down,
            up,
            home,
            end,
            page_up,
            page_down,
            flip,
            tree,
            moves,
            annotate,
            engine,
            review,
            refs,
            online_ref,
            players,
            date,
            views,
        ) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::F),
                i.key_pressed(Key::T),
                i.key_pressed(Key::M),
                i.key_pressed(Key::A),
                i.key_pressed(Key::E),
                i.key_pressed(Key::R),
                i.key_pressed(Key::C),
                i.key_pressed(Key::O),
                i.key_pressed(Key::P),
                i.key_pressed(Key::D),
                i.key_pressed(Key::V),
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
        if page_up {
            self.goto(0);
        }
        if page_down {
            self.go_end();
        }
        if up || down {
            let n = self.session.len();
            if n > 0 {
                let current = self
                    .current_slot
                    .and_then(|id| self.session.position_of(id))
                    .unwrap_or(0);
                let next = if down {
                    (current + 1) % n
                } else {
                    (current + n - 1) % n
                };
                if let Some(id) = self.session.id_at(next) {
                    self.select_slot(id);
                }
            }
        }
        if flip {
            self.orientation = self.orientation.other();
        }
        if tree {
            self.focus_right_tab(RightTab::Tree);
        }
        if moves {
            self.focus_right_tab(RightTab::Moves);
        }
        if annotate && self.current.is_some() && self.recent_preview.is_none() {
            self.focus_right_tab(RightTab::Moves);
            self.annotation_editor = Some(self.node);
            self.annotation_focus = true;
        }
        if engine {
            self.focus_right_tab(RightTab::Engine);
        }
        if review {
            self.focus_right_tab(RightTab::Engine);
            if self.engine_view == EngineView::Cloud {
                self.status = "Game review is unavailable in Cloud-only mode.".to_string();
            } else {
                self.review_open = true;
            }
        }
        if refs {
            self.focus_right_tab(RightTab::References);
        }
        if online_ref {
            self.online_reference_open = !self.online_reference_open;
        }
        if players {
            self.left_tab = LeftTab::Players;
        }
        if date && self.current.is_some() {
            self.date_picker_anchor = None;
            self.open_date_picker();
        }
        if views {
            self.views_open = !self.views_open;
        }
    }

    fn menu_bar(&mut self, root: &mut Ui) {
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open database…").clicked() {
                        if let Some(path) = open_dialog(self.last_database_dir.as_deref()) {
                            self.load_file(&path);
                        }
                        ui.close();
                    }
                    if ui.button("Open database view…").clicked() {
                        if let Some(path) = open_dialog(self.last_database_dir.as_deref()) {
                            self.open_database_view(&path);
                        }
                        ui.close();
                    }
                    if ui.button("Merge database into current list…").clicked() {
                        if let Some(path) = open_dialog(self.last_database_dir.as_deref())
                            && let Ok(library) = open_library(&path)
                        {
                            self.pending_merge = Some(PendingMerge {
                                library,
                                target: DropTarget::Main,
                                label: path.display().to_string(),
                            });
                        }
                        ui.close();
                    }
                    if ui.button("New game from position…").clicked() {
                        self.open_setup_dialog();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.session.is_empty(),
                            egui::Button::new("Save database"),
                        )
                        .on_hover_text(
                            "Save in the current database format. A new database asks for a format.",
                        )
                        .clicked()
                    {
                        self.save_database_now(false);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.session.is_empty(),
                            egui::Button::new("Save database as…"),
                        )
                        .on_hover_text(
                            "Choose PGN, SCID4, classic CBH, or 2CBH. 2CBH writes moves, variations, and metadata only.",
                        )
                        .clicked()
                    {
                        self.save_database_as_now(false);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.current.is_some(),
                            egui::Button::new("Export current game as Typst…"),
                        )
                        .on_hover_text(
                            "Write print-ready Typst source with SVG diagrams, and optionally compile a PDF",
                        )
                        .clicked()
                    {
                        self.start_print_export();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.current.is_some() || !self.session.is_empty(),
                            egui::Button::new("Classify openings…"),
                        )
                        .on_hover_text(
                            "Classify game mainlines using the CC0 lichess-org/chess-openings dataset and write ECO and Opening tags",
                        )
                        .clicked()
                    {
                        self.classify_openings_open = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.library.is_some(),
                            egui::Button::new("Set current database as default reference"),
                        )
                        .on_hover_text("Use the open database as the default preparation reference")
                        .clicked()
                    {
                        self.set_current_database_as_reference();
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
                    if ui.button("Import from ChessABC…").clicked() {
                        self.online_source = OnlineSource::ChessAbc;
                        self.online_open = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Or drop a .pgn, .si4, .si3, .cbh, or .2cbh onto the window.");
                });

                ui.menu_button("Board", |ui| {
                    if ui.button("Flip board (F)").clicked() {
                        self.orientation = self.orientation.other();
                        ui.close();
                    }
                    if ui.button("Copy FEN").clicked() {
                        if let Some(game) = self.current.as_ref() {
                            let fen = fen_string(&game.nodes[self.node].pos);
                            copy_to_clipboard(ui.ctx(), &fen);
                            self.status = "FEN copied to clipboard.".to_string();
                        }
                        ui.close();
                    }
                    if ui.button("Start position (Home)").clicked() {
                        self.goto(0);
                        ui.close();
                    }
                    if ui.button("Set up position…").clicked() {
                        self.open_setup_dialog();
                        ui.close();
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui
                        .checkbox(&mut self.show_pgn_tags, "Show PGN tags")
                        .on_hover_text("Show the editable PGN-tags card above the move list.")
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_annotations, "Show annotations")
                        .on_hover_text(
                            "Show the move annotations and comment editor below the move list.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.online_reference_open, "Show online reference")
                        .on_hover_text(
                            "Show the Lichess opening explorer and cloud evaluation side panel.",
                        )
                        .changed()
                    {
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.photos.is_some(),
                            egui::Checkbox::new(
                                &mut self.show_player_photos,
                                "Show player photos",
                            ),
                        )
                        .on_hover_text("Show portraits in board strips when a photo pack is present.")
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_clocks, "Show clocks")
                        .on_hover_text("Show clock times in board strips when the game has clocks.")
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.engine_bar, "Show evaluation bar")
                        .on_hover_text("Show the vertical evaluation bar beside the board.")
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_nav_buttons, "Show navigation buttons")
                        .on_hover_text(
                            "Show Start, Previous, Next, and End buttons below the board.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_empty_pgn_tags, "Show empty PGN tags")
                        .on_hover_text("Show PGN fields that are absent or empty.")
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut self.hover_moves, "Preview moves on hover")
                        .on_hover_text(
                            "Preview the board position when hovering moves in the notation list.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                });

                ui.menu_button("Settings", |ui| {
                    if ui
                        .button("Settings…")
                        .on_hover_text(
                            "Ratings, correspondence database, and the Lichess API token",
                        )
                        .clicked()
                    {
                        self.open_settings_window();
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button("Export all settings…")
                        .on_hover_text(
                            "Export a single portable JSON bundle of all preferences and configurations",
                        )
                        .clicked()
                    {
                        let default_path = documents_path().join("brett_settings.json");
                        if let Some(target) = save_database_dialog(
                            &default_path,
                            "Export All Settings",
                            "JSON settings bundle",
                            "*.json",
                            true,
                        ) {
                            if let Err(e) =
                                crate::settings::SettingsBundle::current().export_to_file(&target)
                            {
                                eprintln!("Failed to export settings: {e}");
                            } else {
                                self.status = format!("Settings exported to {}.", target.display());
                            }
                        }
                        ui.close();
                    }
                    if ui
                        .button("Import all settings…")
                        .on_hover_text("Import settings bundle from another Brett installation")
                        .clicked()
                    {
                        if let Some(source) = open_file_dialog(
                            "Import Settings Bundle",
                            None,
                            "JSON settings bundle",
                            "*.json",
                        ) {
                            match crate::settings::SettingsBundle::import_from_file(&source) {
                                Ok(bundle) => {
                                    self.apply_imported_settings(&bundle);
                                }
                                Err(e) => {
                                    eprintln!("Failed to import settings: {e}");
                                    self.status = format!("Failed to import settings: {e}");
                                }
                            }
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut self.hover_moves, "Preview moves on hover")
                        .on_hover_text(
                            "Show the position after a move while hovering it in the open game's move list. Off by default; tree, engine, and reference previews stay enabled.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.photos.is_some(),
                            egui::Checkbox::new(
                                &mut self.show_player_photos,
                                "Show player photos",
                            ),
                        )
                        .on_hover_text(
                            "Portraits come from the optional Scid Player_photos pack, derived from scid/PlayerData/Player_photos.zip. The photo data is not part of Brett's MIT-licensed source and is not redistributed with Brett.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    let mut photo_size = self.photo_size;
                    let mut champion_lineage = self.champion_lineage;
                    let mut display_changed = false;
                    ui.add_enabled_ui(self.photos.is_some(), |ui| {
                        ui.menu_button(
                            format!("Photo size: {}", photo_size.label()),
                            |ui| {
                                for size in PhotoSize::ALL {
                                    if ui
                                        .radio_value(&mut photo_size, size, size.label())
                                        .changed()
                                    {
                                        display_changed = true;
                                    }
                                }
                            },
                        )
                        .response
                        .on_hover_text(
                            "Small is 38 px, Medium is 54 px, Large is 72 px. The Players list keeps a fixed 34 px thumbnail.",
                        );
                    });
                    ui.menu_button(
                        format!("Champion lineage: {}", champion_lineage.label()),
                        |ui| {
                            for lineage in ChampionLineage::ALL {
                                if ui
                                    .radio_value(&mut champion_lineage, lineage, lineage.label())
                                    .changed()
                                {
                                    display_changed = true;
                                }
                            }
                        },
                    )
                    .response
                    .on_hover_text(
                        "FIDE/inclusive includes the four knockout champions from 1999–2006. Classical/PCA follows the uninterrupted line from Steinitz onward.",
                    );
                    if display_changed {
                        self.photo_size = photo_size;
                        self.champion_lineage = champion_lineage;
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_nav_buttons, "Show navigation buttons")
                        .on_hover_text(
                            "Show Start, Previous, Next, and End beside the board. Keyboard navigation always works.",
                        )
                        .changed()
                    {
                        self.save_display_prefs();
                    }
                    if ui
                        .checkbox(&mut self.show_pgn_tags, "Show PGN tags")
                        .on_hover_text("Show the editable PGN-tags card above the move list.")
                        .changed()
                    {
                        self.save_display_prefs();
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
                        if ui
                            .small_button("Views")
                            .on_hover_text("Open additional databases side by side")
                            .clicked()
                        {
                            self.views_open = true;
                        }
                    });
                });
                ui.separator();
                match self.left_tab {
                    LeftTab::Games => self.games_content(ui),
                    LeftTab::Players => self.players_content(ui),
                }
            });
    }

    fn online_reference_panel(&mut self, root: &mut Ui) {
        let mut close = false;
        let mut load = false;
        let mut selected = None;
        let loading = self.online_ref_rx.is_some();
        egui::Panel::right("online-reference")
            .default_size(330.0)
            .size_range(260.0..=460.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Online opponent");
                    if loading {
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Compare a Lichess or Chess.com account with the database and the position references.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("online-reference-source")
                        .selected_text(self.online_ref_source.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.online_ref_source,
                                OnlineSource::Lichess,
                                "Lichess",
                            );
                            ui.selectable_value(
                                &mut self.online_ref_source,
                                OnlineSource::ChessCom,
                                "Chess.com",
                            );
                            ui.selectable_value(
                                &mut self.online_ref_source,
                                OnlineSource::ChessAbc,
                                "ChessABC",
                            );
                        });
                    let width = (ui.available_width() - 58.0).max(80.0);
                    ui.add_sized(
                        [width, 22.0],
                        egui::TextEdit::singleline(&mut self.online_ref_username)
                            .hint_text(self.online_ref_source.input_hint())
                            .desired_width(width),
                    );
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!loading, egui::Button::new("Load games"))
                        .clicked()
                    {
                        load = true;
                    }
                    if !self.online_ref_games.is_empty() && ui.small_button("Clear").clicked() {
                        self.online_ref_games.clear();
                        self.online_ref_status = "Online reference cleared.".to_string();
                    }
                });
                ui.label(
                    RichText::new(
                        "Public game data; no password is needed. ChessABC accepts a player name or FIDE id. Optional defaults: lichess_username / chesscom_username in references.json.",
                    )
                    .small()
                    .weak(),
                );
                ui.label(RichText::new(&self.online_ref_status).small().weak());
                ui.separator();

                if self.online_ref_games.is_empty() {
                    ui.label(
                        RichText::new(
                            "Load an account to see its recent games. Click a game to load it read-only on the board.",
                        )
                        .weak(),
                    );
                    return;
                }

                let username = self.online_ref_username.trim().to_lowercase();
                let current_key = self
                    .current
                    .as_ref()
                    .map(|game| position::position_key(&game.nodes[self.node].pos));
                let mut wins = 0usize;
                let mut draws = 0usize;
                let mut losses = 0usize;
                for game in &self.online_ref_games {
                    let is_white = game.white().to_lowercase() == username;
                    let is_black = game.black().to_lowercase() == username;
                    if !is_white && !is_black {
                        continue;
                    }
                    let result = game.result();
                    let player_won = (is_white && result == "1-0") || (is_black && result == "0-1");
                    let player_lost =
                        (is_white && result == "0-1") || (is_black && result == "1-0");
                    if player_won {
                        wins += 1;
                    } else if player_lost {
                        losses += 1;
                    } else if result == "1/2-1/2" {
                        draws += 1;
                    }
                }
                ui.label(
                    RichText::new(format!(
                        "{} · W {wins} · D {draws} · L {losses}",
                        games_label(self.online_ref_games.len())
                    ))
                    .strong(),
                );
                ui.checkbox(
                    &mut self.online_ref_at_position,
                    "Only games reaching the current position",
                );

                let mut indices: Vec<usize> = (0..self.online_ref_games.len()).collect();
                if self.online_ref_at_position
                    && let Some(key) = current_key
                {
                    indices.retain(|&index| {
                        self.online_ref_games[index]
                            .nodes
                            .iter()
                            .any(|node| position::position_key(&node.pos) == key)
                    });
                }
                indices.sort_by(|a, b| {
                    self.online_ref_games[*b]
                        .date()
                        .cmp(self.online_ref_games[*a].date())
                });
                if indices.is_empty() {
                    ui.label(
                        RichText::new("No loaded game reaches this position.").weak().italics(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("online-reference-games")
                        .auto_shrink([false, false])
                        .show_rows(ui, 44.0, indices.len(), |ui, range| {
                            for &index in &indices[range] {
                                let game = &self.online_ref_games[index];
                                let is_white = game.white().to_lowercase() == username;
                                let is_black = game.black().to_lowercase() == username;
                                let (prefix, white, black, suffix, subtitle) = if is_white || is_black {
                                    let (opponent, opponent_color, color) = if is_white {
                                        (game.black(), Color::Black, "White")
                                    } else {
                                        (game.white(), Color::White, "Black")
                                    };
                                    (
                                        "vs ",
                                        RowName::new(opponent, player_title(game, opponent_color))
                                            .with_country(
                                                player_country(game, opponent_color)
                                                    .map(|c| c.badge),
                                            )
                                            .with_fide_id(player_fide_id(game, opponent_color)),
                                        None,
                                        format!(" · as {color}"),
                                        format!(
                                            "{} · {} · {}",
                                            game.result(),
                                            game.event(),
                                            game.date()
                                        ),
                                    )
                                } else {
                                    (
                                        "",
                                        RowName::new(
                                            game.white(),
                                            player_title(game, Color::White),
                                        )
                                        .with_country(
                                            player_country(game, Color::White).map(|c| c.badge),
                                        )
                                        .with_fide_id(player_fide_id(game, Color::White)),
                                        Some(
                                            RowName::new(
                                                game.black(),
                                                player_title(game, Color::Black),
                                            )
                                            .with_country(
                                                player_country(game, Color::Black).map(|c| c.badge),
                                            )
                                            .with_fide_id(player_fide_id(game, Color::Black)),
                                        ),
                                        String::new(),
                                        format!("{} · {}", game.result(), game.date()),
                                    )
                                };
                                let width = ui.available_width();
                                if game_row(
                                    ui,
                                    false,
                                    prefix,
                                    white,
                                    black,
                                    &suffix,
                                    &subtitle,
                                    width,
                                    42.0,
                                )
                                .clicked()
                                {
                                    selected = Some(index);
                                }
                            }
                        });
                }
            });

        if close {
            self.online_reference_open = false;
        }
        if load {
            self.start_online_reference();
        }
        if let Some(index) = selected
            && let Some(game) = self.online_ref_games.get(index).cloned()
        {
            let label = format!("Online game · {} vs {}", game.white(), game.black());
            self.open_game_preview(game, label);
        }
    }

    fn games_content(&mut self, ui: &mut Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("filter players, event…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        let rows = self.session.filter(&self.filter);

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
        let mut reorder = None;
        let mut external_drop = None;
        let mut external_merge = None;
        egui::ScrollArea::vertical()
            .id_salt("game-list")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for &id in &rows[range] {
                    let Some(slot) = self.session.slot(id) else {
                        continue;
                    };
                    let white_title = title_from_tag(slot.header("WhiteTitle"));
                    let black_title = title_from_tag(slot.header("BlackTitle"));
                    let white_fide_id = player_fide_id_from_header(slot.header("WhiteFideId"));
                    let black_fide_id = player_fide_id_from_header(slot.header("BlackFideId"));
                    let white_country = player_country_from_header(
                        slot.header("WhiteCountry")
                            .or_else(|| slot.header("WhiteFed")),
                    );
                    let black_country = player_country_from_header(
                        slot.header("BlackCountry")
                            .or_else(|| slot.header("BlackFed")),
                    );
                    let subtitle = format!("{} · {}", slot.event(), slot.date());
                    let selected = self.current_slot == Some(id) && self.prep.is_none();
                    let width = ui.available_width();
                    let response = game_row(
                        ui,
                        selected,
                        "",
                        RowName::new(slot.white(), white_title)
                            .with_country(white_country.map(|c| c.badge))
                            .with_fide_id(white_fide_id.as_deref()),
                        Some(
                            RowName::new(slot.black(), black_title)
                                .with_country(black_country.map(|c| c.badge))
                                .with_fide_id(black_fide_id.as_deref()),
                        ),
                        "",
                        &subtitle,
                        width,
                        row_h - 2.0,
                    );
                    response.dnd_set_drag_payload(GameRowDrag { id });

                    let pointer = ui.ctx().pointer_interact_pos();
                    let hovering_drag = response.dnd_hover_payload::<GameRowDrag>().is_some()
                        || response.dnd_hover_payload::<ViewGameDrag>().is_some()
                        || response.dnd_hover_payload::<ViewDatabaseDrag>().is_some();
                    let drop_before = hovering_drag
                        && pointer.is_some_and(|pos| pos.y < response.rect.center().y);
                    if hovering_drag {
                        let y = if drop_before {
                            response.rect.top()
                        } else {
                            response.rect.bottom()
                        };
                        ui.painter().line_segment(
                            [
                                egui::pos2(response.rect.left() + 4.0, y),
                                egui::pos2(response.rect.right() - 4.0, y),
                            ],
                            egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                        );
                    }
                    let insertion = self
                        .session
                        .position_of(id)
                        .map(|position| if drop_before { position } else { position + 1 });
                    if let Some(payload) = response.dnd_release_payload::<GameRowDrag>()
                        && let Some(insertion) = insertion
                    {
                        reorder = Some((payload.id, insertion));
                    }
                    if let Some(payload) = response.dnd_release_payload::<ViewGameDrag>()
                        && let Some(insertion) = insertion
                    {
                        external_drop = Some((payload.view, payload.id, insertion));
                    }
                    if let Some(payload) = response.dnd_release_payload::<ViewDatabaseDrag>() {
                        external_merge = Some(payload.view);
                    }

                    if response.clicked() {
                        select = Some(id);
                    }
                }
            });
        if let Some((id, insertion)) = reorder
            && self.session.move_slot(id, insertion)
        {
            self.session_dirty = true;
            self.status = "Game moved in the current list.".to_string();
        }
        if let Some((view_index, game_id, insert_at)) = external_drop
            && let Some(game) = self.views.get(view_index).and_then(|view| {
                view.session
                    .load(game_id, &std::collections::HashMap::new())
                    .ok()
            })
        {
            self.pending_game_drop = Some(PendingGameDrop {
                label: format!(
                    "Game from {}",
                    self.views
                        .get(view_index)
                        .map(DatabaseView::name)
                        .unwrap_or_else(|| "database view".to_string())
                ),
                game,
                target: DropTarget::Main,
                insert_at,
            });
        }
        if let Some(view_index) = external_merge
            && let Some(view) = self.views.get(view_index)
        {
            self.pending_merge = Some(PendingMerge {
                library: view.library.clone(),
                target: DropTarget::Main,
                label: view.name(),
            });
        }
        if let Some(id) = select {
            self.select_slot(id);
        }
    }

    fn players_content(&mut self, ui: &mut Ui) {
        let Some(index) = self.selected_player else {
            self.player_list(ui);
            return;
        };
        let Some(player) = self.players.get(index).cloned() else {
            self.selected_player = None;
            self.reset_wiki();
            return;
        };
        if self.wiki_enabled && self.wiki_for.as_deref() != Some(player.key.as_str()) {
            self.open_wiki_for(&player);
        }
        let ctx = ui.ctx().clone();
        let portrait = self.photo_texture(&ctx, &player.name);
        let champion = champions::champion(&player.name, self.champion_lineage);
        let mut open_photo = false;

        let prep_color = self
            .prep
            .as_ref()
            .filter(|prep| prep.player_key == player.key)
            .map(|prep| prep.opponent_color());
        let game_indices: Vec<usize> = player
            .game_indices
            .iter()
            .copied()
            .filter(|index| {
                let Some(color) = prep_color else {
                    return true;
                };
                self.library
                    .as_ref()
                    .and_then(|library| library.get(*index))
                    .is_some_and(|entry| {
                        let white = players::player_key(entry.white()).as_deref()
                            == Some(player.key.as_str());
                        let black = players::player_key(entry.black()).as_deref()
                            == Some(player.key.as_str());
                        if color == Color::White { white } else { black }
                    })
            })
            .collect();
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
            if let Some(texture) = portrait.as_ref() {
                let response = ui
                    .add(
                        egui::Image::from_texture(texture)
                            .fit_to_exact_size(vec2(
                                self.photo_size.pixels(),
                                self.photo_size.pixels(),
                            ))
                            .sense(Sense::click()),
                    )
                    .on_hover_text("Open larger photo");
                if response.clicked() {
                    open_photo = true;
                }
            }
            if let Some(title) = player.title {
                ui.label(RichText::new(title.short()).small().strong().color(GOLD))
                    .on_hover_text("Title from the database or imported PGN");
            }
            ui.heading(&player.name);
            if let Some(country) = player.country {
                ui.label(RichText::new(country.badge).small().weak())
                    .on_hover_text(format!("{} ({})", country.name, country.code));
            }
            if let Some(id) = player.fide_id.as_deref()
                && let Some(url) = fide::profile_url(id)
                && ui
                    .link(RichText::new(format!("FIDE {id} ↗")).small().weak())
                    .on_hover_text("Open this player's FIDE rating profile in your browser")
                    .clicked()
            {
                open_browser_url(ui.ctx(), &url);
            }
            if let Some(champion) = champion {
                ui.label(RichText::new("👑").size(20.0).color(GOLD))
                    .on_hover_text(champion.tooltip());
            }
            if top_priority_player_key(&self.prep_player_names).as_deref()
                == Some(player.key.as_str())
            {
                ui.label(RichText::new("You").small().strong().color(GOLD))
                    .on_hover_text("First name in the player-priority list");
            }
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
        self.wiki_summary_section(ui, &player);

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let display_name = natural_player_name(&player.name);
            if ui
                .button("Prepare as White")
                .on_hover_text(format!("You play White; {display_name} is Black"))
                .clicked()
            {
                start_white = true;
            }
            if ui
                .button("Prepare as Black")
                .on_hover_text(format!("You play Black; {display_name} is White"))
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
                        .hint_text("/path/to/masters.pgn, reference.si4, database.cbh, or database.2cbh")
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
                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("My player names").small().strong());
                    if self.prep_player_names.is_empty() {
                        ui.label(RichText::new("not configured").small().weak());
                    } else {
                        ui.label(
                            RichText::new(self.prep_player_names.join(" · "))
                                .small()
                                .weak(),
                        )
                        .on_hover_text("Highest priority first");
                    }
                    if ui.small_button("Edit in Settings…").clicked() {
                        self.open_settings_window();
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
        let total = game_indices.len();
        ui.label(RichText::new(format!("Games ({total})")).strong());
        if let Some(color) = prep_color {
            ui.label(
                RichText::new(format!(
                    "Preparation · showing only {}'s {} games",
                    natural_player_name(&player.name),
                    color_name(color)
                ))
                .small()
                .weak(),
            );
        }
        if total == 0 {
            ui.label(RichText::new("No games for this player.").weak());
        } else {
            egui::ScrollArea::vertical()
                .id_salt("player-games")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, total, |ui, range| {
                    for &idx in &game_indices[range] {
                        let Some(entry) = self.library.as_ref().and_then(|l| l.get(idx)) else {
                            continue;
                        };
                        let player_white = players::player_key(entry.white()).as_deref()
                            == Some(player.key.as_str());
                        let (opponent, opponent_color, color) = if player_white {
                            (entry.black(), Color::Black, "White")
                        } else {
                            (entry.white(), Color::White, "Black")
                        };
                        let opponent_title = title_from_tag(entry.header(match opponent_color {
                            Color::White => "WhiteTitle",
                            Color::Black => "BlackTitle",
                        }));
                        let opponent_fide_id =
                            player_fide_id_from_header(entry.header(match opponent_color {
                                Color::White => "WhiteFideId",
                                Color::Black => "BlackFideId",
                            }));
                        let opponent_country = player_country_from_header(
                            entry
                                .header(match opponent_color {
                                    Color::White => "WhiteCountry",
                                    Color::Black => "BlackCountry",
                                })
                                .or_else(|| {
                                    entry.header(match opponent_color {
                                        Color::White => "WhiteFed",
                                        Color::Black => "BlackFed",
                                    })
                                }),
                        );
                        let suffix = format!(" · as {color}");
                        let subtitle =
                            format!("{} · {} · {}", entry.result(), entry.event(), entry.date());
                        let selected = self
                            .session
                            .source_slot(idx)
                            .is_some_and(|id| self.current_slot == Some(id))
                            && self.prep.is_none();
                        let width = ui.available_width();
                        if game_row(
                            ui,
                            selected,
                            "vs ",
                            RowName::new(opponent, opponent_title)
                                .with_country(opponent_country.map(|c| c.badge))
                                .with_fide_id(opponent_fide_id.as_deref()),
                            None,
                            &suffix,
                            &subtitle,
                            width,
                            row_h - 2.0,
                        )
                        .clicked()
                        {
                            select_game = Some(idx);
                        }
                    }
                });
        }

        if open_photo {
            self.photo_viewer = Some(player.name.clone());
        }
        if back {
            if prep_matches(self, &player.key) {
                exit = true;
            }
            self.selected_player = None;
            self.reset_wiki();
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
            self.select_source(index);
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
            ui.label(RichText::new(&self.players_status).weak());
            if self.players_rx.is_some() {
                ui.spinner();
                ui.label(
                    RichText::new(
                        "The game list is already usable; the player directory is being built in the background.",
                    )
                    .weak()
                    .small(),
                );
            }
            return;
        }
        let rows = self.players.filter(&self.player_filter);
        if rows.is_empty() {
            ui.label(RichText::new("No matching players.").weak());
            return;
        }

        let you_key = top_priority_player_key(&self.prep_player_names);
        let row_h = 46.0;
        let ctx = ui.ctx().clone();
        let mut select = None;
        let mut view_photo = None;
        egui::ScrollArea::vertical()
            .id_salt("player-list")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for &index in &rows[range] {
                    let Some(player) = self.players.get(index) else {
                        continue;
                    };
                    let name = player.name.clone();
                    let key = player.key.clone();
                    let player_title = player.title.map(titles::PlayerTitle::short);
                    let player_country = player.country.map(|c| c.badge);
                    let player_fide_id = player.fide_id.clone();
                    let is_you = you_key.as_deref() == Some(key.as_str());
                    let title = if is_you {
                        "You".to_string()
                    } else {
                        name.clone()
                    };
                    let subtitle = format!(
                        "{}{} · score {:.0}% · Elo {} · {}",
                        if is_you {
                            format!("{name} · ")
                        } else {
                            String::new()
                        },
                        games_label(player.games() as usize),
                        player.score() * 100.0,
                        player
                            .elo_avg()
                            .map(|elo| format!("{elo:.0}"))
                            .unwrap_or_else(|| "—".to_string()),
                        player.years_label()
                    );
                    let selected = self.selected_player == Some(index);
                    let champion = champions::champion(&name, self.champion_lineage);
                    let photo = self.photo_texture(&ctx, &name);
                    let width = ui.available_width();
                    let (response, photo_clicked) = player_row(
                        ui,
                        selected,
                        &title,
                        player_title,
                        player_country,
                        player_fide_id.as_deref(),
                        &subtitle,
                        width,
                        photo.as_ref(),
                        champion,
                    );
                    if photo_clicked {
                        view_photo = Some(name);
                    } else if response.clicked() {
                        select = Some(index);
                    }
                }
            });
        if let Some(name) = view_photo {
            self.photo_viewer = Some(name);
        }
        if let Some(index) = select {
            self.select_player(index);
        }
    }

    fn right_tab_visible(&self, tab: RightTab) -> bool {
        if self.multi_view {
            self.visible_tabs[tab.index()]
        } else {
            self.right_tab == tab
        }
    }

    fn focus_right_tab(&mut self, tab: RightTab) {
        self.right_tab = tab;
        if self.multi_view {
            self.visible_tabs[tab.index()] = true;
        }
    }

    fn moves_panel(&mut self, root: &mut Ui) {
        egui::Panel::right("moves")
            .default_size(390.0)
            .size_range(280.0..=560.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for tab in RightTab::ALL {
                        if self.multi_view {
                            let visible = self.visible_tabs[tab.index()];
                            let response = ui.selectable_label(visible, tab.label()).on_hover_text(
                                if visible {
                                    "Visible in the multi-section view · click to hide"
                                } else {
                                    "Click to show this section"
                                },
                            );
                            if response.clicked() {
                                let visible_count =
                                    self.visible_tabs.iter().filter(|visible| **visible).count();
                                if !visible || visible_count > 1 {
                                    self.visible_tabs[tab.index()] = !visible;
                                }
                                if self.visible_tabs[tab.index()] {
                                    self.right_tab = tab;
                                } else if self.right_tab == tab {
                                    self.right_tab = RightTab::ALL
                                        .iter()
                                        .copied()
                                        .find(|candidate| self.visible_tabs[candidate.index()])
                                        .unwrap_or(RightTab::Moves);
                                }
                            }
                        } else {
                            ui.selectable_value(&mut self.right_tab, tab, tab.label());
                        }
                    }
                    ui.separator();
                    if ui
                        .selectable_label(self.multi_view, "Multi")
                        .on_hover_text(
                            "Show several sections at once. Click a tab to add or remove it.",
                        )
                        .clicked()
                    {
                        self.multi_view = !self.multi_view;
                        if self.multi_view {
                            let current = self.right_tab.index();
                            self.visible_tabs = [false; 4];
                            self.visible_tabs[current] = true;
                        } else {
                            self.right_tab = RightTab::ALL
                                .iter()
                                .copied()
                                .find(|candidate| self.visible_tabs[candidate.index()])
                                .unwrap_or(self.right_tab);
                        }
                    }
                });
                ui.separator();
                if self.multi_view {
                    let visible: Vec<RightTab> = RightTab::ALL
                        .iter()
                        .copied()
                        .filter(|tab| self.visible_tabs[tab.index()])
                        .collect();
                    let available = ui.available_height();
                    let section_height = ((available - 10.0 * visible.len() as f32)
                        / visible.len() as f32)
                        .max(220.0);
                    egui::ScrollArea::vertical()
                        .id_salt("multi-sections")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (index, tab) in visible.iter().enumerate() {
                                if index > 0 {
                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.add_space(4.0);
                                }
                                ui.allocate_ui_with_layout(
                                    vec2(ui.available_width(), section_height),
                                    Layout::top_down(Align::Min),
                                    |ui| self.section_content(*tab, ui),
                                );
                            }
                        });
                } else {
                    self.section_content(self.right_tab, ui);
                }
            });
    }

    fn section_content(&mut self, tab: RightTab, ui: &mut Ui) {
        match tab {
            RightTab::Moves if self.recent_preview.is_some() => self.preview_content(ui),
            RightTab::Moves => {
                egui::ScrollArea::vertical()
                    .id_salt("moves-tab-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.moves_content(ui));
            }
            RightTab::Tree => self.tree_content(ui),
            RightTab::Engine => self.engine_content(ui),
            RightTab::References => self.references_content(ui),
        }
    }

    fn references_content(&mut self, ui: &mut Ui) {
        self.maybe_request_cloud();

        let snapshot = self.cloud.clone();
        let position = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone());
        let status = self.cloud_status.clone();
        let loading = self.cloud_rx.is_some() || self.explorer_rx.is_some();
        let token_configured = self.lichess_token.is_some();
        let mut refresh = false;
        let mut open_settings = false;
        let mut reference_path = self.reference_path.clone();
        let mut reference_enabled = self.reference_enabled;
        let mut choose_reference = false;
        let mut use_current_reference = false;
        let mut set_current_reference = false;
        let current_db = self
            .library
            .as_ref()
            .map(|library| library.path().display().to_string())
            .unwrap_or_else(|| "No database open".to_string());
        let reference_status = self.reference_status.clone();

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

                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(10.0)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.heading("Reference sources");
                        ui.label(
                            RichText::new(
                                "Preparation can use the current database, a stronger reference database, or an online account.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!("Current database · {current_db}"))
                                .small()
                                .weak(),
                        );
                        if ui
                            .button("Use current database")
                            .on_hover_text(
                                "Use the open database as the fallback preparation reference",
                            )
                            .clicked()
                        {
                            use_current_reference = true;
                        }
                        if ui
                            .add_enabled(
                                self.library.is_some(),
                                egui::Button::new("Set open DB as default"),
                            )
                            .on_hover_text(
                                "Persist the open database as the default preparation reference",
                            )
                            .clicked()
                        {
                            set_current_reference = true;
                        }

                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Reference database").small().strong());
                            let width = (ui.available_width() - 92.0).max(80.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut reference_path)
                                    .desired_width(width)
                                    .hint_text("e.g. Mega Database (.pgn, .si4, .cbh, .2cbh)"),
                            );
                            if ui.button("Choose…").clicked() {
                                choose_reference = true;
                            }
                        });
                        ui.checkbox(
                            &mut reference_enabled,
                            "Use the reference database for preparation",
                        );
                        ui.label(RichText::new(&reference_status).small().weak());

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.label(RichText::new("Online accounts").small().strong());
                        ui.horizontal_wrapped(|ui| {
                            for source in [
                                OnlineSource::Lichess,
                                OnlineSource::ChessCom,
                                OnlineSource::ChessAbc,
                            ] {
                                if ui
                                    .button(source.label())
                                    .on_hover_text(format!(
                                        "Open the {} opponent reference panel",
                                        source.label()
                                    ))
                                    .clicked()
                                {
                                    self.online_ref_source = source;
                                    self.online_reference_open = true;
                                }
                            }
                        });
                        ui.label(
                            RichText::new(
                                "Use File → Import to save games from an online account into a database.",
                            )
                            .small()
                            .weak(),
                        );
                    });
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

                if let Err(err) = &snapshot.chessdb {
                    ui.label(
                        RichText::new(format!("chessdb.cn: {err}"))
                            .small()
                            .weak(),
                    );
                }
                if let Err(err) = &snapshot.explorer {
                    ui.add_space(3.0);
                    if token_configured {
                        ui.label(RichText::new(format!("Lichess explorer: {err}")).small().weak());
                    } else {
                        ui.label(
                            RichText::new(
                                "Lichess opening-explorer requests need an API token. The public cloud evaluation works without one.",
                            )
                            .small()
                            .weak(),
                        );
                        if ui.small_button("Add a token in Settings").clicked() {
                            open_settings = true;
                        }
                    }
                }
            });

        if open_settings {
            self.open_settings_window();
        }
        if set_current_reference {
            self.set_current_database_as_reference();
        }
        if use_current_reference {
            reference_path.clear();
            reference_enabled = false;
        }
        if choose_reference && let Some(path) = open_dialog(None) {
            reference_path = path.display().to_string();
            reference_enabled = true;
        }
        if reference_path != self.reference_path || reference_enabled != self.reference_enabled {
            self.reference_path = reference_path;
            self.reference_enabled = reference_enabled;
            self.reference = None;
            self.reference_rx = None;
            self.reference_job = None;
            self.reference_status = if use_current_reference {
                "Using the current database as the reference source.".to_string()
            } else if self.reference_path.trim().is_empty() {
                "No reference database configured — using the current database prior.".to_string()
            } else {
                "Reference database configured. Open Players → Prepare to build its index."
                    .to_string()
            };
            self.save_reference_prefs();
        }
        if refresh {
            self.refresh_cloud();
        }
    }

    fn engine_content(&mut self, ui: &mut Ui) {
        self.maybe_engine_analysis();
        self.maybe_request_cloud();
        egui::ScrollArea::vertical()
            .id_salt("engine-tab")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.engine_content_inner(ui);
            });
    }

    fn engine_content_inner(&mut self, ui: &mut Ui) {
        let mut start = false;
        let mut stop = false;
        let mut settings_changed = false;
        let mut clicked_line: Option<Vec<String>> = None;
        let mut clicked_move: Option<Move> = None;
        let mut browse_engine = false;
        let mut apply_engine = false;
        let mut engine_view = self.engine_view;

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
                            } else if self.engine_slots.iter().any(|s| s.engine.is_some()) {
                                "starting"
                            } else {
                                "stopped"
                            })
                            .small()
                            .weak(),
                        );
                    });
                });
                if engine_view != EngineView::Cloud {
                    ui.horizontal_wrapped(|ui| {
                        for idx in 0..3 {
                            let is_on = self.engine_slots[idx].enabled;
                            let name = self.engine_slots[idx].short_name(idx);
                            let btn_text = if is_on {
                                format!("[{}] {} 🟢", idx + 1, name)
                            } else {
                                format!("[{}] {} ⚫", idx + 1, name)
                            };
                            let resp = ui.selectable_label(is_on, btn_text);
                            if resp.clicked() {
                                self.toggle_engine_slot(idx);
                            }
                            resp.on_hover_text(format!(
                                "Hotkey {}: Toggle Engine {} on/off",
                                idx + 1,
                                idx + 1
                            ));
                        }
                    });
                    ui.add_space(2.0);
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
                }
                ui.label(RichText::new(&self.engine_status).small().weak());
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Show").small().weak());
                    ui.selectable_value(&mut engine_view, EngineView::Local, "Local");
                    ui.selectable_value(&mut engine_view, EngineView::Cloud, "Cloud");
                    ui.selectable_value(&mut engine_view, EngineView::Both, "Both");
                });
                ui.add_space(4.0);

                if engine_view != EngineView::Cloud {
                    ui.horizontal(|ui| {
                        let arrow = if self.engine_settings_open { "▾" } else { "▸" };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{arrow}  Engine and hardware")).strong(),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Local engine executable and hardware resources")
                            .clicked()
                        {
                            self.engine_settings_open = !self.engine_settings_open;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let review_running =
                                self.review.as_ref().is_some_and(|review| review.running);
                            if review_running {
                                ui.spinner();
                            }
                            let label = if self.review_open {
                                "Game review ▾"
                            } else {
                                "Game review"
                            };
                            if ui
                                .selectable_label(self.review_open, label)
                                .on_hover_text(
                                    "Local-engine game review · grades every mainline move",
                                )
                                .clicked()
                            {
                                self.review_open = !self.review_open;
                            }
                        });
                    });
                    if self.engine_settings_open {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Configure engine:").strong());
                            for idx in 0..3 {
                                let name = self.engine_slots[idx].short_name(idx);
                                let is_sel = self.selected_engine_slot == idx;
                                if ui.selectable_label(is_sel, format!("[{}] {}", idx + 1, name)).clicked() {
                                    self.selected_engine_slot = idx;
                                }
                            }
                        });
                        let sel = self.selected_engine_slot;
                        ui.horizontal(|ui| {
                            ui.label("Executable");
                            let width = (ui.available_width() - 8.0).max(120.0);
                            let hint = if sel == 0 {
                                "/usr/bin/stockfish"
                            } else {
                                "Path to engine executable"
                            };
                            ui.add(
                                egui::TextEdit::singleline(&mut self.engine_slots[sel].path)
                                    .desired_width(width)
                                    .hint_text(hint),
                            )
                            .on_hover_text(format!("Path to UCI-compatible engine for Slot {}", sel + 1));
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
                                self.engine_slots[sel].path = crate::engine::default_engine_path()
                                    .display()
                                    .to_string();
                                if sel == 0 {
                                    self.engine_path = self.engine_slots[0].path.clone();
                                }
                            }
                            if ui
                                .button("Apply & restart")
                                .on_hover_text(
                                    "Save the executable, threads, and hash size, then restart local engines",
                                )
                                .clicked()
                            {
                                apply_engine = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Threads");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_slots[sel].threads)
                                    .range(1..=256)
                                    .speed(1),
                            )
                            .on_hover_text(
                                "UCI Threads option. Applied when the engine advertises it.",
                            );
                            ui.label("Hash / RAM");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_slots[sel].hash_mb)
                                    .range(1..=32_768)
                                    .suffix(" MB")
                                    .speed(64),
                            )
                            .on_hover_text(
                                "UCI Hash option: transposition-table memory in megabytes.",
                            );
                        });
                        if let Some(engine) = &self.engine_slots[sel].engine {
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
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .checkbox(&mut self.cache_engine_analysis, "Cache engine analysis")
                                .on_hover_text("Persist locally analyzed positions to SQLite cache")
                                .changed()
                            {
                                apply_engine = true;
                            }
                            if ui
                                .small_button("ℹ")
                                .on_hover_text("About local analysis storage and migration")
                                .clicked()
                            {
                                self.show_cache_info = true;
                            }
                            if ui
                                .checkbox(&mut self.heuristic_supersede, "Smart Stockfish replacement")
                                .on_hover_text(
                                    "Use fine-grained formula to supersede earlier/weaker analysis (disable to keep all)",
                                )
                                .changed()
                            {
                                apply_engine = true;
                            }
                        });
                        let cache_opt = self.analysis_cache.clone();
                        if let Some(cache) = cache_opt {
                            let count = cache.count_positions();
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(format!("Cached positions: {count}"))
                                        .small()
                                        .weak(),
                                );
                                if ui
                                    .button("Export cache…")
                                    .on_hover_text("Export compacted SQLite snapshot to a backup file")
                                    .clicked()
                                {
                                    let default_path = documents_path().join("brett_analysis_backup.db");
                                    if let Some(target) = save_database_dialog(
                                        &default_path,
                                        "Export Analysis Cache",
                                        "SQLite database",
                                        "*.db",
                                        true,
                                    ) {
                                        if let Err(e) = cache.export_to(&target) {
                                            eprintln!("Failed to export cache: {e}");
                                        } else {
                                            self.status = format!("Cache exported to {}.", target.display());
                                        }
                                    }
                                }
                                if ui
                                    .button("Import cache…")
                                    .on_hover_text("Import and merge positions from an external analysis SQLite database")
                                    .clicked()
                                    && let Some(source) = open_file_dialog(
                                        "Import Analysis Cache",
                                        None,
                                        "SQLite database",
                                        "*.db",
                                    )
                                {
                                    match cache.import_from(&source, self.heuristic_supersede) {
                                        Ok(imported) => {
                                            self.status = format!("Imported {imported} positions from {}.", source.display());
                                            self.cached_analyses.clear();
                                            self.cached_analyses_pos = None;
                                            self.update_cached_analyses();
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to import cache: {e}");
                                            self.status = format!("Failed to import cache: {e}");
                                        }
                                    }
                                }
                                if ui
                                    .button("Clear cache")
                                    .on_hover_text("Delete all cached engine evaluations")
                                    .clicked()
                                {
                                    let _ = cache.clear();
                                    self.cached_analyses.clear();
                                    self.cached_analyses_pos = None;
                                }
                            });
                        }
                        if self.engine_settings_pending() {
                            ui.label(
                                RichText::new("Settings changed — Apply & restart to use them.")
                                    .small()
                                    .color(GOLD),
                            );
                        } else if !self.engine_slots.iter().any(|s| s.engine.is_some()) {
                            ui.label(
                                RichText::new(
                                    "The engine starts when analysis is requested or Apply is pressed.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                    }
                    ui.add_space(4.0);

                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .checkbox(&mut self.engine_auto, "Auto analyze")
                            .on_hover_text("Re-analyze when the position changes")
                            .changed()
                        {
                            settings_changed = true;
                            self.save_display_prefs();
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
                        let prev_mode = self.engine_limit_mode;
                        egui::ComboBox::from_id_salt("engine-limit")
                            .selected_text(self.engine_limit_mode.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.engine_limit_mode,
                                    EngineLimitMode::Infinite,
                                    "Infinite",
                                );
                                ui.selectable_value(
                                    &mut self.engine_limit_mode,
                                    EngineLimitMode::Depth,
                                    "Depth",
                                );
                                ui.selectable_value(
                                    &mut self.engine_limit_mode,
                                    EngineLimitMode::Time,
                                    "Time",
                                );
                            });
                        if self.engine_limit_mode != prev_mode {
                            self.save_display_prefs();
                        }
                        match self.engine_limit_mode {
                            EngineLimitMode::Infinite => {
                                ui.label(
                                    RichText::new("Runs continuously until stopped")
                                        .small()
                                        .weak(),
                                );
                            }
                            EngineLimitMode::Depth => {
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut self.engine_depth)
                                            .range(1..=100)
                                            .speed(1)
                                            .prefix("d"),
                                    )
                                    .changed()
                                {
                                    self.save_display_prefs();
                                }
                            }
                            EngineLimitMode::Time => {
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut self.engine_time_ms)
                                            .range(100..=60_000)
                                            .suffix(" ms")
                                            .speed(100),
                                    )
                                    .changed()
                                {
                                    self.save_display_prefs();
                                }
                            }
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
                        if !self.engine_slots.iter().any(|s| s.engine.is_some()) && ui.button("Start engine").clicked() {
                            start = true;
                        }

                        let curr_hash = self
                            .current_position()
                            .map(|p| p.position_hash())
                            .unwrap_or(0);
                        if let Some((notice_hash, depth, ts)) = self.engine_cache_saved_notice {
                            if notice_hash == curr_hash && ts.elapsed() < Duration::from_secs(4) {
                                ui.label(
                                    RichText::new(format!("✓ Saved to cache (d{depth})"))
                                        .color(Color32::from_rgb(74, 222, 128))
                                        .strong()
                                        .small(),
                                );
                            } else if let Some(max_cached) =
                                self.cached_analyses.iter().map(|a| a.depth).max()
                            {
                                ui.label(
                                    RichText::new(format!("💾 Stored in cache (d{max_cached})"))
                                        .weak()
                                        .small(),
                                );
                            }
                        } else if let Some(max_cached) =
                            self.cached_analyses.iter().map(|a| a.depth).max()
                        {
                            ui.label(
                                RichText::new(format!("💾 Stored in cache (d{max_cached})"))
                                    .weak()
                                    .small(),
                            );
                        }
                    });
                } else {
                    ui.label(
                        RichText::new(
                            "Cloud-only mode: the local engine, hardware controls, and game review are off.",
                        )
                        .small()
                        .color(GOLD),
                    );
                    if ui.button("Enable local engine").clicked() {
                        engine_view = EngineView::Both;
                    }
                }
            });

        let previous_view = self.engine_view;
        self.engine_view = engine_view;
        if previous_view != engine_view {
            self.save_display_prefs();
            if engine_view == EngineView::Cloud {
                self.drop_local_engines();
            }
        }
        if browse_engine && let Some(path) = open_engine_dialog() {
            let sel = self.selected_engine_slot;
            self.engine_slots[sel].path = path.display().to_string();
            if sel == 0 {
                self.engine_path = path.display().to_string();
            }
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
            self.stop_all_engines();
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

        let mut hover_preview = None;
        if engine_view != EngineView::Cloud {
            let active_slots: Vec<usize> = (0..3)
                .filter(|&i| self.engine_slots[i].enabled)
                .collect();
            if active_slots.is_empty() {
                ui.label(
                    RichText::new("No engines enabled. Press 1, 2, or 3 to toggle an engine.")
                        .weak()
                        .italics(),
                );
            } else {
                for &idx in &active_slots {
                    let slot = &self.engine_slots[idx];
                    let slot_name = slot.short_name(idx);
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(6.0)
                        .stroke(Stroke::new(1.0, BORDER))
                        .inner_margin(6.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("[{}] {}", idx + 1, slot_name))
                                        .strong(),
                                );
                                if slot.searching {
                                    ui.spinner();
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(if slot.ready {
                                            "ready"
                                        } else if slot.engine.is_some() {
                                            "starting"
                                        } else {
                                            "stopped"
                                        })
                                        .small()
                                        .weak(),
                                    );
                                });
                            });
                            let infos: Vec<EngineInfo> = slot.infos.values().cloned().collect();
                            if infos.is_empty() {
                                ui.label(
                                    RichText::new(if slot.searching {
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
                                    let steps = pv_steps(&pos, &info.pv, 14);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format!("PV {}", info.multipv))
                                                .small()
                                                .weak(),
                                        );
                                        ui.label(RichText::new(score).monospace().strong().color(color));
                                        ui.label(RichText::new(format!("d{}", info.depth)).small().weak());
                                        if steps.is_empty() {
                                            ui.label(RichText::new("…").weak());
                                        }
                                        pv_steps_ui(ui, &steps, &info.pv, &mut hover_preview, &mut clicked_line);
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
                                    ui.add_space(2.0);
                                }
                            }

                            if let Some(best) = &slot.bestmove {
                                ui.label(
                                    RichText::new(format!("bestmove {best}"))
                                        .monospace()
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                    ui.add_space(4.0);
                }
            }
        }

        if engine_view != EngineView::Cloud && self.review_open {
            ui.add_space(8.0);
            self.review_content(ui);
        }

        if engine_view != EngineView::Local {
            if engine_view == EngineView::Both {
                ui.add_space(8.0);
            }
            let (cloud_line, cloud_preview, show_cache_info) = cloud_engine_section(
                ui,
                self.cloud.as_ref(),
                &self.cached_analyses,
                Some(&pos),
                &self.cloud_status,
            );
            if cloud_line.is_some() {
                clicked_line = cloud_line;
            }
            if cloud_preview.is_some() {
                hover_preview = cloud_preview;
            }
            if show_cache_info {
                self.show_cache_info = true;
            }
        }

        if self.correspondence_enabled {
            ui.add_space(8.0);
            let (correspondence_move, correspondence_preview) = correspondence_section(
                ui,
                self.correspondence.as_ref(),
                &pos,
                &self.correspondence_status,
            );
            if correspondence_move.is_some() {
                clicked_move = correspondence_move;
            }
            if correspondence_preview.is_some() {
                hover_preview = correspondence_preview;
            }
        }

        if let Some(line) = clicked_line {
            self.play_uci_line(&line);
        } else if let Some(mv) = clicked_move {
            self.play_move(mv);
        } else if let Some(preview) = hover_preview {
            self.hover_preview = Some(preview);
        }
    }

    fn review_content(&mut self, ui: &mut Ui) {
        let running = self.review.as_ref().is_some_and(|review| review.running)
            || self.review_book_rx.is_some();
        let can_write_nags = self.recent_preview.is_none();
        let mut start = false;
        let mut cancel = false;
        let mut write_nags = false;
        let mut prefs_changed = false;

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
                        if ui.small_button("Hide").clicked() {
                            self.review_open = false;
                        }
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
                        if ui
                            .checkbox(&mut self.review_skip_book, "Skip opening book")
                            .on_hover_text(
                                "Skip leading book moves (open book → local opening tree → Lichess Masters)",
                            )
                            .changed()
                        {
                            prefs_changed = true;
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

        if prefs_changed {
            self.save_display_prefs();
        }
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
                1.0
            } else {
                done as f32 / total as f32
            };
            ui.add_space(8.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(format!("{done} / {total} positions analyzed"))
                    .desired_width(ui.available_width()),
            );

            if review.book_plies > 0 {
                ui.label(
                    RichText::new(format!(
                        "Opening book: {} skipped",
                        moves_label(full_moves(review.book_plies))
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text(format!(
                    "{} leading plies matched the opening book and are excluded from move grades and accuracy.",
                    review.book_plies
                ));
            }

            let summary = review::accuracy_summary(&moves);
            ui.horizontal_wrapped(|ui| {
                let tooltip = "Lichess-style win-probability accuracy (volatility-weighted + harmonic mean) and average centipawn loss. Opening-book and forced moves are excluded; this is an engine estimate, not an official rating.";
                if summary.white.is_some() || summary.black.is_some() {
                    let w_str = summary
                        .white
                        .map(|w| format!("{w:.1}%"))
                        .unwrap_or_else(|| "—".to_string());
                    let b_str = summary
                        .black
                        .map(|b| format!("{b:.1}%"))
                        .unwrap_or_else(|| "—".to_string());
                    let prefix = if running { "Accuracy (partial)" } else { "Accuracy" };
                    ui.label(
                        RichText::new(format!("{prefix}: White {w_str} · Black {b_str}"))
                            .strong()
                            .color(GOLD),
                    )
                    .on_hover_text(tooltip);

                    if summary.white_acpl.is_some() || summary.black_acpl.is_some() {
                        let wa = summary
                            .white_acpl
                            .map(|a| format!("{a:.0}"))
                            .unwrap_or_else(|| "—".to_string());
                        let ba = summary
                            .black_acpl
                            .map(|a| format!("{a:.0}"))
                            .unwrap_or_else(|| "—".to_string());
                        ui.label(
                            RichText::new(format!("ACPL {wa} · {ba}"))
                                .small()
                                .weak(),
                        )
                        .on_hover_text(tooltip);
                    }
                } else {
                    ui.label(RichText::new("Accuracy pending").weak());
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
        let mut tag_add_value: Option<(&'static str, String)> = None;
        let mut date_clicked: Option<egui::Pos2> = None;
        let mut hover_preview = None;
        let mut diagram_toggle = None;

        game_header_card(
            ui,
            &game,
            prep.as_ref(),
            self.rating_priority,
            self.show_all_ratings,
            &mut date_clicked,
        );
        ui.add_space(6.0);
        let mut show_empty = self.show_empty_pgn_tags;
        if self.show_pgn_tags {
            let tags_ui = ui.scope(|ui| {
                game_tags_card(
                    ui,
                    &game,
                    &mut tag_update,
                    &mut tag_delete,
                    &mut tag_add,
                    &mut tag_add_value,
                    &mut self.tags_expanded,
                    &mut show_empty,
                    &mut self.board_tag_visible,
                    &self.forced_tag_visibility,
                    &mut date_clicked,
                );
            });
            tags_ui.response.context_menu(|ui| {
                if ui.button("Hide PGN tags").clicked() {
                    self.show_pgn_tags = false;
                    self.save_display_prefs();
                    ui.close();
                }
            });
            if show_empty != self.show_empty_pgn_tags {
                self.show_empty_pgn_tags = show_empty;
                self.save_display_prefs();
            }
            ui.add_space(8.0);
        }

        let tokens = domain::tokens(&game);
        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(12.0)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Moves").size(15.0).strong());
                    ui.label(
                        RichText::new(format!("{} moves", full_moves(game.mainline_len())))
                            .weak()
                            .small(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        result_badge(ui, game.result());
                        if ui
                            .small_button("Annotate")
                            .on_hover_text("Edit the selected move")
                            .clicked()
                        {
                            open_editor = Some(node);
                        }
                    });
                });
                ui.add_space(4.0);

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
                    .max_height((ui.available_height() * 0.44).clamp(120.0, 360.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if tokens.is_empty() {
                            ui.label(
                                RichText::new("No moves yet. Play a move on the board.")
                                    .weak()
                                    .italics(),
                            );
                            return;
                        }
                        render_move_tokens_flat(
                            ui,
                            &tokens,
                            &game,
                            node,
                            &mut clicked,
                            &mut open_editor,
                            &mut nag_action,
                            &mut hover_preview,
                            self.hover_moves,
                            &self.print_diagrams,
                            &mut diagram_toggle,
                        );
                    });
            });

        if let Some(node) = diagram_toggle {
            toggle_diagram(&mut self.print_diagrams, node);
        }

        let editing = editor.is_some();
        if self.show_annotations || editing {
            ui.add_space(8.0);

            let edit_node = editor
                .unwrap_or(node)
                .min(game.nodes.len().saturating_sub(1));
            let selected_san = game.nodes[edit_node]
                .mv
                .as_ref()
                .map(|ply| domain::figurine_san(&ply.san))
                .unwrap_or_else(|| "Start position".to_string());
            let selected_nags = game.nodes[edit_node].nags.clone();
            let symbols = nag_symbols(&selected_nags);
            let mut comment = game.nodes[edit_node].comment.clone();

            let ann_response = egui::Frame::new()
                .fill(SURFACE)
                .corner_radius(12.0)
                .stroke(Stroke::new(
                    if editing { 1.5 } else { 1.0 },
                    if editing { GOLD } else { BORDER },
                ))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Annotations").size(15.0).strong());
                        accent_chip(ui, &selected_san);
                        if !symbols.is_empty() {
                            ui.label(RichText::new(symbols).strong().color(GOLD));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if editing && ui.small_button("Done").clicked() {
                                close_editor = true;
                            }
                            if ui
                                .add_enabled(
                                    !selected_nags.is_empty(),
                                    egui::Button::new("Clear").small(),
                                )
                                .on_hover_text("Remove all symbols from this move")
                                .clicked()
                            {
                                nag_action = Some((edit_node, None));
                            }
                        });
                    });
                    ui.label(
                        RichText::new(if editing {
                            "Editing · click a symbol to toggle it"
                        } else {
                            "Click a move, then choose its symbols"
                        })
                        .small()
                        .weak(),
                    );
                    ui.add_space(5.0);

                    if edit_node > 0 {
                        if let Some(nag) = nag_palette(ui, &selected_nags, false) {
                            nag_action = Some((edit_node, Some(nag)));
                        }
                    } else {
                        ui.label(
                            RichText::new("Annotations belong to moves; select a move first.")
                                .small()
                                .weak()
                                .italics(),
                        );
                    }

                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Comment").small().strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new("saved automatically").small().weak());
                        });
                    });
                    egui::Frame::new()
                        .fill(Color32::from_rgb(0x1A, 0x1C, 0x1F))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            let response = ui.add_sized(
                                [width, 38.0],
                                egui::TextEdit::multiline(&mut comment)
                                    .id_salt(("annotation-comment", edit_node))
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::ZERO)
                                    .hint_text("Add a note for this move…")
                                    .desired_rows(2)
                                    .desired_width(width),
                            );
                            if response.changed() {
                                comment_update = Some((edit_node, comment.clone()));
                            }
                            if focus_comment && edit_node == node {
                                response.request_focus();
                            }
                        });
                });
            ann_response.response.context_menu(|ui| {
                if ui.button("Hide annotations").clicked() {
                    self.show_annotations = false;
                    self.save_display_prefs();
                    ui.close();
                }
            });
        }

        if tag_update.is_some()
            || tag_delete.is_some()
            || tag_add.is_some()
            || tag_add_value.is_some()
        {
            if let Some(game) = self.current.as_mut() {
                if let Some((index, value)) = tag_update
                    && let Some((_, current)) = game.headers.get_mut(index)
                {
                    *current = value;
                }
                if let Some(index) = tag_delete
                    && index < game.headers.len()
                {
                    let (name, _) = game.headers.remove(index);
                    self.forced_tag_visibility
                        .remove(&name.to_ascii_lowercase());
                    if name.eq_ignore_ascii_case("Round") {
                        self.board_tag_visible = false;
                    }
                }
                if let Some(key) = tag_add {
                    if key == "Board" {
                        if game.header("Round").is_none() {
                            game.set_header("Round", "?".to_string());
                        }
                        self.board_tag_visible = true;
                    } else {
                        game.set_header(key, String::new());
                    }
                    self.forced_tag_visibility.insert(key.to_ascii_lowercase());
                }
                if let Some((key, value)) = tag_add_value {
                    game.set_header(key, value);
                    self.forced_tag_visibility.insert(key.to_ascii_lowercase());
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
        if let Some(anchor) = date_clicked {
            self.date_picker_anchor = Some(anchor);
            self.open_date_picker();
        }
        if let Some(preview) = hover_preview {
            self.hover_preview = Some(preview);
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
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            ui.heading("No game");
            return;
        };
        let key = position::position_key(&pos);
        if self.recent_preview.is_none()
            && let Some(prep) = self.prep.clone()
        {
            self.prep_tree(ui, &prep, &pos, key);
            return;
        }

        self.maybe_request_cloud();
        let mut explorer_source = self.explorer_source;
        let mut source_changed = None;
        let mut choose_tree = false;
        let mut use_current_tree = false;
        let mut clicked = None;
        let mut clicked_uci = None;
        let mut hover_preview = None;
        let mut load_game_id = None;
        let mut choose_book = false;
        let mut clear_book = false;
        let mut build_index = false;
        let book = self.book.clone();
        let book_status = self.book_status.clone();

        egui::ScrollArea::vertical()
            .id_salt("tree-tab")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Tree");
                    if self.cloud_rx.is_some() || self.explorer_rx.is_some() {
                        ui.spinner();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Opening book").small().weak());
                    let width = (ui.available_width() - 176.0).max(80.0);
                    let path = if self.book_path.trim().is_empty() {
                        "No opening book selected".to_string()
                    } else {
                        self.book_path.clone()
                    };
                    ui.add_sized(
                        [width, 18.0],
                        egui::Label::new(RichText::new(&path).monospace().small()).truncate(),
                    )
                    .on_hover_text(path);
                    if ui.button("Choose…").clicked() {
                        choose_book = true;
                    }
                    if ui
                        .button("Clear")
                        .on_hover_text("Stop showing an opening book")
                        .clicked()
                    {
                        clear_book = true;
                    }
                });
                ui.label(RichText::new(&book_status).small().weak());
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Opening tree file").small().weak());
                    let width = (ui.available_width() - 176.0).max(80.0);
                    let path = if self.tree_path.trim().is_empty() {
                        "Using the current database".to_string()
                    } else {
                        self.tree_path.clone()
                    };
                    ui.add_sized(
                        [width, 18.0],
                        egui::Label::new(RichText::new(&path).monospace().small()).truncate(),
                    )
                    .on_hover_text(path);
                    if ui.button("Choose…").clicked() {
                        choose_tree = true;
                    }
                    if ui
                        .button("Current DB")
                        .on_hover_text("Use the currently open database for the opening tree")
                        .clicked()
                    {
                        use_current_tree = true;
                    }
                });
                ui.label(RichText::new(&self.tree_status).small().weak());
                ui.add_space(4.0);

                let tree_index = self.tree_index.as_ref().or(self.index.as_ref());
                let tree_label = if self.tree_index.is_some() {
                    self.tree_library
                        .as_ref()
                        .map(|library| library.path().display().to_string())
                        .unwrap_or_else(|| "opening-tree file".to_string())
                } else {
                    "current database".to_string()
                };
                ui.label(
                    RichText::new(format!("Local opening tree · {tree_label}"))
                        .strong()
                        .small(),
                );
                if let Some(index) = tree_index {
                    clicked = opening_tree_table(
                        ui,
                        Some(index),
                        key,
                        &pos,
                        "No indexed game reached this position.",
                        self.trend_config(),
                        &mut hover_preview,
                    );
                } else {
                    ui.label(RichText::new(&self.index_status).weak().small());
                    if self.index_rx.is_some() || self.tree_index_rx.is_some() {
                        ui.label(
                            RichText::new(
                                "Building the position index in the background. You can keep browsing.",
                            )
                            .weak()
                            .small(),
                        );
                    }
                    if self.index_build_button(ui) {
                        build_index = true;
                    }
                }

                if let Some(book) = &book {
                    let moves = book.moves(&pos).unwrap_or_default();
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(SURFACE)
                        .corner_radius(10.0)
                        .stroke(Stroke::new(1.0, BORDER))
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.heading("Opening book");
                                ui.label(RichText::new(book.summary()).small().weak());
                            });
                            ui.label(
                                RichText::new(
                                    "Native book moves · hover to preview · click to play",
                                )
                                .small()
                                .weak(),
                            );
                            ui.add_space(3.0);
                            if moves.is_empty() {
                                ui.label(
                                    RichText::new("No book move for this position.")
                                        .weak()
                                        .italics(),
                                );
                            } else {
                                egui::Grid::new("tree-book-grid")
                                    .num_columns(6)
                                    .striped(true)
                                    .spacing([10.0, 4.0])
                                    .show(ui, |ui| {
                                        for heading in [
                                            "Move", "Games", "Weight", "Score", "W/D/B",
                                            "Note",
                                        ] {
                                            ui.label(
                                                RichText::new(heading).strong().small(),
                                            );
                                        }
                                        ui.end_row();
                                        for mv in &moves {
                                            let san = SanPlus::from_move(pos.clone(), mv.mv)
                                                .to_string();
                                            let response = ui
                                                .selectable_label(false, san)
                                                .on_hover_text(
                                                    "Hover to preview · click to play",
                                                );
                                            if response.hovered() {
                                                hover_preview =
                                                    preview_after_move(&pos, mv.mv);
                                            }
                                            if response.clicked() {
                                                clicked = Some(mv.mv);
                                            }
                                            ui.label(compact_count(mv.games));
                                            ui.label(compact_count(mv.weight));
                                            ui.label(
                                                mv.score_white()
                                                    .map(|score| {
                                                        format!("{:.0}%", score * 100.0)
                                                    })
                                                    .unwrap_or_else(|| "—".to_string()),
                                            );
                                            ui.label(format!(
                                                "{}/{}/{}",
                                                compact_count(mv.white),
                                                compact_count(mv.draw),
                                                compact_count(mv.black)
                                            ));
                                            let mut note = book_annotation_label(
                                                mv.annotation,
                                                mv.recommendation,
                                            );
                                            if let Some(quality) = mv.quality {
                                                if !note.is_empty() {
                                                    note.push(' ');
                                                }
                                                note.push(quality);
                                            }
                                            ui.label(
                                                RichText::new(note).small().color(GOLD),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            }
                        });
                }

                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(10.0)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Lichess reference");
                            for source in [
                                cloud::ExplorerSource::Masters,
                                cloud::ExplorerSource::Lichess,
                                cloud::ExplorerSource::Both,
                            ] {
                                if ui
                                    .selectable_label(
                                        explorer_source == source,
                                        source.short_label(),
                                    )
                                    .on_hover_text(source.label())
                                    .clicked()
                                {
                                    explorer_source = source;
                                    source_changed = Some(source);
                                }
                            }
                            if self.cloud_rx.is_some() || self.explorer_rx.is_some() {
                                ui.spinner();
                            }
                        });
                        if let Some(snapshot) = &self.cloud {
                            match &snapshot.explorer {
                                Ok(explorer) => {
                                    if let Some(opening) = &explorer.opening {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} · {}",
                                                opening.eco, opening.name
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    }
                                    if let Some(note) = &explorer.note {
                                        ui.label(
                                            RichText::new(note).small().color(GOLD),
                                        );
                                    }
                                    if explorer.moves.is_empty() {
                                        ui.label(
                                            RichText::new("No Lichess games reached this position.")
                                                .weak()
                                                .italics(),
                                        );
                                    } else {
                                        egui::Grid::new("tree-lichess-grid")
                                            .num_columns(5)
                                            .striped(true)
                                            .spacing([10.0, 4.0])
                                            .show(ui, |ui| {
                                                for heading in
                                                    ["Move", "Games", "Score", "W/D/B", "Avg Elo"]
                                                {
                                                    ui.label(
                                                        RichText::new(heading).strong().small(),
                                                    );
                                                }
                                                ui.end_row();
                                                for mv in explorer.moves.iter().take(30) {
                                                    let response = ui
                                                        .selectable_label(false, &mv.san)
                                                        .on_hover_text(
                                                            "Hover to preview · click to play",
                                                        );
                                                    if response.hovered() {
                                                        hover_preview = preview_after_uci(
                                                            &pos, &mv.uci,
                                                        );
                                                    }
                                                    if response.clicked() {
                                                        clicked_uci = Some(mv.uci.clone());
                                                    }
                                                    ui.label(compact_count(mv.total()));
                                                    ui.label(format!(
                                                        "{:.0}%",
                                                        mv.score() * 100.0
                                                    ));
                                                    ui.label(format!(
                                                        "{}/{}/{}",
                                                        compact_count(mv.white),
                                                        compact_count(mv.draws),
                                                        compact_count(mv.black)
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
                                }
                                Err(err) => {
                                    ui.label(
                                        RichText::new(format!("Lichess explorer: {err}"))
                                            .small()
                                            .weak(),
                                    );
                                }
                            }
                            if let Ok(explorer) = &snapshot.explorer
                                && !explorer.top_games.is_empty()
                            {
                                ui.add_space(8.0);
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Lichess top games").strong().small(),
                                    );
                                    ui.label(
                                        RichText::new("click to load onto the board")
                                            .small()
                                            .weak(),
                                    );
                                });
                                for game in &explorer.top_games {
                                    let date = match (game.year, game.month) {
                                        (Some(year), Some(month)) => {
                                            format!(" · {year}-{month:02}")
                                        }
                                        (Some(year), None) => format!(" · {year}"),
                                        _ => String::new(),
                                    };
                                    let white = match game.white_rating {
                                        Some(rating) => {
                                            format!("{} ({rating})", game.white)
                                        }
                                        None => game.white.clone(),
                                    };
                                    let black = match game.black_rating {
                                        Some(rating) => {
                                            format!("{} ({rating})", game.black)
                                        }
                                        None => game.black.clone(),
                                    };
                                    let label = format!("{white} – {black}{date}");
                                    if ui
                                        .selectable_label(false, label)
                                        .on_hover_text(
                                            "Load this game read-only onto the board",
                                        )
                                        .clicked()
                                    {
                                        load_game_id = Some(game.id.clone());
                                    }
                                }
                            }
                        } else {
                            ui.label(RichText::new(&self.cloud_status).weak().small());
                        }
                    });
            });

        if build_index {
            self.request_index_build();
        }
        if choose_tree && let Some(path) = open_dialog(None) {
            self.tree_path = path.display().to_string();
            self.save_display_prefs();
            self.start_tree_index(path);
        }
        if use_current_tree {
            self.tree_path.clear();
            self.tree_index = None;
            self.tree_library = None;
            self.tree_index_rx = None;
            self.tree_index_job = None;
            self.tree_status = "Using the current database for the opening tree.".to_string();
            self.save_display_prefs();
        }
        if choose_book && let Some(path) = open_dialog(None) {
            self.book_path = path.display().to_string();
            self.save_display_prefs();
            self.start_book(path);
        }
        if clear_book {
            self.book_path.clear();
            self.book = None;
            self.book_rx = None;
            self.book_job = None;
            self.book_status = "No opening book loaded.".to_string();
            self.save_display_prefs();
        }
        if let Some(source) = source_changed {
            self.explorer_source = source;
            if let Err(err) = crate::settings::CloudPrefs::save_explorer_source(source) {
                self.cloud_status = format!("Could not save the explorer choice: {err:#}");
            }
            if let Some(fen) = self.current_fen() {
                if self.cloud.is_some() {
                    self.start_explorer_refresh(fen);
                } else {
                    self.cloud_pending = None;
                    self.start_cloud(fen);
                }
            }
        }
        if let Some(id) = load_game_id {
            self.start_lichess_game(id);
        }
        if let Some(uci) = clicked_uci {
            self.play_uci(&uci);
        } else if let Some(mv) = clicked {
            self.play_move(mv);
        } else if let Some(preview) = hover_preview {
            self.hover_preview = Some(preview);
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
        book.recent_games_for(key, prep.opponent_color(), limit)
            .into_iter()
            .filter_map(|game| {
                let indexed = library.get(game.index)?;
                Some(PrepRecentRow {
                    index: game.index,
                    white: indexed.white().to_string(),
                    black: indexed.black().to_string(),
                    white_title: title_from_tag(indexed.header("WhiteTitle")),
                    black_title: title_from_tag(indexed.header("BlackTitle")),
                    white_country: player_country_from_header(
                        indexed
                            .header("WhiteCountry")
                            .or_else(|| indexed.header("WhiteFed")),
                    )
                    .map(|c| c.badge),
                    black_country: player_country_from_header(
                        indexed
                            .header("BlackCountry")
                            .or_else(|| indexed.header("BlackFed")),
                    )
                    .map(|c| c.badge),
                    white_fide_id: player_fide_id_from_header(indexed.header("WhiteFideId")),
                    black_fide_id: player_fide_id_from_header(indexed.header("BlackFideId")),
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

    fn prep_tree(&mut self, ui: &mut Ui, prep: &PrepSession, pos: &crate::chess::Chess, key: u64) {
        let mut exit = false;
        ui.horizontal(|ui| {
            ui.heading("Player tree");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Exit prep").clicked() {
                    exit = true;
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("vs").weak());
            if let Some(title) = prep.player_title {
                ui.label(RichText::new(title.short()).small().strong().color(GOLD));
            }
            ui.label(RichText::new(&prep.player_name).weak());
            if let Some(id) = &prep.player_fide_id {
                ui.label(RichText::new(format!("· {id}")).small().weak())
                    .on_hover_text(format!("FIDE ID {id}"));
            }
            ui.label(
                RichText::new(format!(
                    "· You play {} · board from {}",
                    color_name(prep.our_color),
                    color_name(self.orientation)
                ))
                .weak(),
            );
        });
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
        let recent_rows = self.prep_recent_rows(prep, key, 500);
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
        let mut recent_limit_changed = false;
        let mut build_index = false;
        let online_rows = self
            .prep_online_games
            .iter()
            .filter(|game| {
                let name = if prep.opponent_color() == Color::White {
                    game.white()
                } else {
                    game.black()
                };
                players::player_key(name).as_deref() == Some(prep.player_key.as_str())
            })
            .take(recent_limit)
            .map(|game| {
                (
                    format!("{} · {} · {}", game.date(), game.event(), game.result()),
                    game.clone(),
                )
            })
            .collect::<Vec<_>>();
        let online_status = self.prep_online_status.clone();
        let online_loading = self.prep_online_rx.is_some();
        let mut load_online = false;
        let mut clear_online = false;
        let mut online_preview = None;
        let mut clicked = None;
        let mut preview = None;
        let mut hover_preview = None;

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
                                let response = ui
                                    .selectable_label(false, text)
                                    .on_hover_text("Hover to preview · click to play");
                                if response.hovered() {
                                    hover_preview = preview_after_move(pos, row.mv);
                                }
                                if response.clicked() {
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
                                self.trend_config(),
                                &mut hover_preview,
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
                            if self.index_build_button(ui) {
                                build_index = true;
                            }
                        }
                    });

                ui.add_space(4.0);
                egui::CollapsingHeader::new(RichText::new("Recent games").strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Show last");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut recent_limit)
                                        .range(1..=50)
                                        .speed(1.0),
                                )
                                .changed()
                            {
                                recent_limit_changed = true;
                            }
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
                                if game_row(
                                    ui,
                                    false,
                                    "",
                                    RowName::new(&row.white, row.white_title)
                                        .with_country(row.white_country)
                                        .with_fide_id(row.white_fide_id.as_deref()),
                                    Some(
                                        RowName::new(&row.black, row.black_title)
                                            .with_country(row.black_country)
                                            .with_fide_id(row.black_fide_id.as_deref()),
                                    ),
                                    "",
                                    &row.subtitle,
                                    width,
                                    40.0,
                                )
                                .clicked()
                                {
                                    preview = Some(row.index);
                                }
                            }
                            if recent_rows.len() > recent_limit {
                                ui.label(
                                    RichText::new(format!(
                                        "Showing {} of {} recent games.",
                                        recent_limit,
                                        recent_rows.len()
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        }

                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("ChessABC · latest games").strong());
                            if ui
                                .add_enabled(
                                    !online_loading,
                                    egui::Button::new("Load latest"),
                                )
                                .on_hover_text(
                                    "Fetch the prepared player's newest games from ChessABC",
                                )
                                .clicked()
                            {
                                load_online = true;
                            }
                            if !self.prep_online_games.is_empty()
                                && ui.small_button("Clear").clicked()
                            {
                                clear_online = true;
                            }
                            if online_loading {
                                ui.spinner();
                            }
                        });
                        if !online_status.is_empty() {
                            ui.label(RichText::new(&online_status).small().weak());
                        }
                        if online_rows.is_empty() {
                            if !online_status.is_empty() && !online_loading {
                                ui.label(
                                    RichText::new(format!(
                                        "No recent {} games for {} on ChessABC.",
                                        color_name(prep.opponent_color()),
                                        prep.player_name
                                    ))
                                    .weak()
                                    .italics(),
                                );
                            }
                        } else {
                            let width = (ui.available_width() - 4.0).max(100.0);
                            for (subtitle, game) in &online_rows {
                                if game_row(
                                    ui,
                                    false,
                                    "",
                                    RowName::new(
                                        game.white(),
                                        player_title(game, Color::White),
                                    )
                                    .with_country(
                                        player_country(game, Color::White).map(|c| c.badge),
                                    )
                                    .with_fide_id(player_fide_id(game, Color::White)),
                                    Some(
                                        RowName::new(
                                            game.black(),
                                            player_title(game, Color::Black),
                                        )
                                        .with_country(
                                            player_country(game, Color::Black).map(|c| c.badge),
                                        )
                                        .with_fide_id(player_fide_id(game, Color::Black)),
                                    ),
                                    "",
                                    subtitle,
                                    width,
                                    40.0,
                                )
                                .clicked()
                                {
                                    online_preview = Some(game.clone());
                                }
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

        if build_index {
            self.request_index_build();
        }
        self.prep_recent_limit = recent_limit;
        if recent_limit_changed {
            self.save_display_prefs();
        }
        if clear_online {
            self.prep_online_games.clear();
            self.prep_online_status = "ChessABC games cleared.".to_string();
        }
        if load_online {
            self.start_prep_online();
        }
        if let Some(game) = online_preview {
            self.open_game_preview(game, "ChessABC · latest game");
        } else if let Some(mv) = clicked {
            self.play_move(mv);
        } else if let Some(index) = preview {
            self.open_recent_preview(index);
        } else if let Some(preview) = hover_preview {
            self.hover_preview = Some(preview);
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
        match library.load(index) {
            Ok(game) => self.open_game_preview(game, format!("Game {} · read only", index + 1)),
            Err(err) => {
                self.status = format!("Could not open game {}: {err}", index + 1);
            }
        }
    }

    /// Open any game read-only while preserving the game and cursor that were
    /// on the board. Used by recent-game rows and Lichess top games.
    fn open_game_preview(&mut self, game: Game, label: impl Into<String>) {
        if self.recent_preview.is_some() {
            return;
        }
        let return_game = self.current.clone();
        let return_slot = self.current_slot;
        let return_node = self.node;
        let return_tab = self.right_tab;
        let return_orientation = self.orientation;
        let return_print_diagrams = self.print_diagrams.clone();
        if self.dirty
            && let (Some(slot), Some(current)) = (self.current_slot, self.current.take())
        {
            self.edited.insert(slot, current);
        }
        self.clear_review();
        self.clear_engine_results();
        self.annotation_editor = None;
        self.annotation_focus = false;
        let label = label.into();
        self.recent_preview = Some(RecentPreview {
            return_game,
            return_slot,
            return_node,
            return_tab,
            return_orientation,
            return_print_diagrams,
            label: label.clone(),
        });
        self.reset_branch_state();
        self.print_diagrams.clear();
        self.current = Some(game);
        self.current_slot = None;
        self.dirty = false;
        let orientation = self
            .current
            .as_ref()
            .and_then(|game| my_player_color(&self.prep_player_names, game));
        if let Some(color) = orientation {
            self.orientation = color;
        }
        self.focus_right_tab(RightTab::Moves);
        self.goto(0);
        self.status = format!("Previewing {label}");
    }

    fn close_recent_preview(&mut self) {
        let Some(preview) = self.recent_preview.take() else {
            return;
        };
        // Review and engine results belong to the preview, not to the game
        // that is about to be restored.
        self.clear_review();
        self.clear_engine_results();
        self.reset_branch_state();
        self.current = preview.return_game;
        self.current_slot = preview.return_slot;
        self.print_diagrams = preview.return_print_diagrams;
        self.dirty = false;
        self.orientation = preview.return_orientation;
        self.focus_right_tab(preview.return_tab);
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

        let preview_label = self
            .recent_preview
            .as_ref()
            .map(|preview| preview.label.clone())
            .unwrap_or_else(|| "Game preview".to_string());
        ui.horizontal(|ui| {
            ui.label(RichText::new(preview_label).strong().color(GOLD));
            ui.label(RichText::new("read only").small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Close preview").clicked() {
                    close = true;
                }
            });
        });
        ui.label(
            RichText::new(if self.prep.is_some() {
                "Preparation is paused; closing returns to the same position."
            } else {
                "Closing returns to the game that was on the board."
            })
            .weak()
            .small(),
        );
        ui.add_space(6.0);
        let mut date_clicked: Option<egui::Pos2> = None;
        game_header_card(
            ui,
            &game,
            None,
            self.rating_priority,
            self.show_all_ratings,
            &mut date_clicked,
        );
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
                        RichText::new(format!("{} moves", full_moves(game.mainline_len())))
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

    /// Play a principal variation from the current node, reusing existing
    /// children where possible and adding the rest to the game tree.
    fn play_uci_line(&mut self, moves: &[String]) {
        for uci in moves {
            self.play_uci(uci);
        }
    }

    fn bottom_bar(&mut self, root: &mut Ui) {
        if !self.show_nav_buttons {
            return;
        }
        egui::Panel::bottom("nav").show(root, |ui| {
            ui.add_space(4.0);
            let nav_response = ui.horizontal(|ui| {
                if ui
                    .button("|<")
                    .on_hover_text("Start (Home / PageUp)")
                    .clicked()
                {
                    self.goto(0);
                }
                if ui.button("<").on_hover_text("Previous move (←)").clicked() {
                    self.prev_move();
                }
                if ui.button(">").on_hover_text("Next move (→)").clicked() {
                    self.next_move();
                }
                if ui
                    .button(">|")
                    .on_hover_text("End (End / PageDown)")
                    .clicked()
                {
                    self.go_end();
                }
            });
            nav_response.response.context_menu(|ui| {
                if ui.button("Hide navigation buttons").clicked() {
                    self.show_nav_buttons = false;
                    self.save_display_prefs();
                    ui.close();
                }
            });
            ui.add_space(4.0);
        });
    }

    fn database_views_window(&mut self, ctx: &egui::Context) {
        if !self.views_open {
            return;
        }
        let mut open = self.views_open;
        let mut open_request = false;
        let mut close_view = None;
        let mut preview = None;
        let mut drop_from_view = None;
        let mut drop_from_main = None;
        let mut reorder_view = None;
        let mut merge_from_view = None;
        let mut merge_to_main = None;

        egui::Window::new("Database views")
            .open(&mut open)
            .default_size([470.0, 620.0])
            .min_width(360.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Open database…").clicked() {
                        open_request = true;
                    }
                    ui.label(
                        RichText::new(
                            "Drag a game row, or the ⣿ database handle, onto another list.",
                        )
                        .small()
                        .weak(),
                    );
                });
                ui.add_space(4.0);
                if self.views.is_empty() {
                    ui.label(
                        RichText::new(
                            "No additional databases are open. Open one here, or drop a database onto Brett and choose “Open in database views”.",
                        )
                        .weak(),
                    );
                    return;
                }

                egui::ScrollArea::vertical()
                    .id_salt("database-views-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for view_index in 0..self.views.len() {
                            let view = &mut self.views[view_index];
                            let name = view.name();
                            let count = view.session.len();
                            ui.horizontal(|ui| {
                                let handle = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new("⣿")
                                                .color(Color32::from_gray(150))
                                                .size(14.0),
                                        )
                                        .sense(Sense::drag()),
                                    )
                                    .on_hover_text(
                                        "Drag this database onto the current list or another view to merge",
                                    );
                                handle.dnd_set_drag_payload(ViewDatabaseDrag { view: view_index });
                                ui.label(RichText::new(name).strong());
                                ui.label(RichText::new(games_label(count)).small().weak());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("Close").clicked() {
                                        close_view = Some(view_index);
                                    }
                                    if ui
                                        .small_button("Merge into current")
                                        .on_hover_text(
                                            "Append distinct games from this database to the current list",
                                        )
                                        .clicked()
                                    {
                                        merge_to_main = Some(view_index);
                                    }
                                });
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Filter").small().weak());
                                ui.add(
                                    egui::TextEdit::singleline(&mut view.filter)
                                        .hint_text("players, event…")
                                        .desired_width(f32::INFINITY),
                                );
                            });
                            let append_zone = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new("Drop a game here to append")
                                            .small()
                                            .weak(),
                                    )
                                    .sense(Sense::hover()),
                                )
                                .on_hover_text("Append a dragged game to this database view");
                            if let Some(payload) = append_zone.dnd_release_payload::<GameRowDrag>() {
                                drop_from_main = Some((payload.id, view_index, count));
                            }
                            if let Some(payload) = append_zone.dnd_release_payload::<ViewGameDrag>()
                                && payload.view != view_index
                            {
                                drop_from_view =
                                    Some((payload.view, payload.id, view_index, count));
                            }

                            let rows = view.session.filter(&view.filter);
                            if rows.is_empty() {
                                ui.label(RichText::new("No games.").weak());
                            } else {
                                let row_h = 42.0;
                                egui::ScrollArea::vertical()
                                    .id_salt(("database-view-games", view_index))
                                    .max_height(230.0)
                                    .auto_shrink([false, false])
                                    .show_rows(ui, row_h, rows.len(), |ui, range| {
                                        for &id in &rows[range] {
                                            let Some(slot) = view.session.slot(id) else {
                                                continue;
                                            };
                                            let white_title =
                                                title_from_tag(slot.header("WhiteTitle"));
                                            let black_title =
                                                title_from_tag(slot.header("BlackTitle"));
                                            let white_fide_id = player_fide_id_from_header(
                                                slot.header("WhiteFideId"),
                                            );
                                            let black_fide_id = player_fide_id_from_header(
                                                slot.header("BlackFideId"),
                                            );
                                            let white_country = player_country_from_header(
                                                slot.header("WhiteCountry")
                                                    .or_else(|| slot.header("WhiteFed")),
                                            );
                                            let black_country = player_country_from_header(
                                                slot.header("BlackCountry")
                                                    .or_else(|| slot.header("BlackFed")),
                                            );
                                            let subtitle =
                                                format!("{} · {}", slot.event(), slot.date());
                                            let width = ui.available_width();
                                            let response = game_row(
                                                ui,
                                                false,
                                                "",
                                                RowName::new(slot.white(), white_title)
                                                    .with_country(white_country.map(|c| c.badge))
                                                    .with_fide_id(white_fide_id.as_deref()),
                                                Some(
                                                    RowName::new(slot.black(), black_title)
                                                        .with_country(
                                                            black_country.map(|c| c.badge),
                                                        )
                                                        .with_fide_id(black_fide_id.as_deref()),
                                                ),
                                                "",
                                                &subtitle,
                                                width,
                                                row_h - 2.0,
                                            );
                                            response.dnd_set_drag_payload(ViewGameDrag {
                                                view: view_index,
                                                id,
                                            });
                                            let pointer = ui.ctx().pointer_interact_pos();
                                            let hovering = response
                                                .dnd_hover_payload::<GameRowDrag>()
                                                .is_some()
                                                || response
                                                    .dnd_hover_payload::<ViewGameDrag>()
                                                    .is_some()
                                                || response
                                                    .dnd_hover_payload::<ViewDatabaseDrag>()
                                                    .is_some();
                                            let drop_before = hovering
                                                && pointer.is_some_and(|pos| {
                                                    pos.y < response.rect.center().y
                                                });
                                            if hovering {
                                                let y = if drop_before {
                                                    response.rect.top()
                                                } else {
                                                    response.rect.bottom()
                                                };
                                                ui.painter().line_segment(
                                                    [
                                                        egui::pos2(
                                                            response.rect.left() + 4.0,
                                                            y,
                                                        ),
                                                        egui::pos2(
                                                            response.rect.right() - 4.0,
                                                            y,
                                                        ),
                                                    ],
                                                    egui::Stroke::new(
                                                        2.0,
                                                        ui.visuals().selection.bg_fill,
                                                    ),
                                                );
                                            }
                                            let Some(position) = view.session.position_of(id) else {
                                                continue;
                                            };
                                            let insertion =
                                                if drop_before { position } else { position + 1 };
                                            if let Some(payload) =
                                                response.dnd_release_payload::<GameRowDrag>()
                                            {
                                                drop_from_main =
                                                    Some((payload.id, view_index, insertion));
                                            }
                                            if let Some(payload) =
                                                response.dnd_release_payload::<ViewGameDrag>()
                                            {
                                                if payload.view == view_index {
                                                    reorder_view =
                                                        Some((view_index, payload.id, insertion));
                                                } else {
                                                    drop_from_view = Some((
                                                        payload.view,
                                                        payload.id,
                                                        view_index,
                                                        insertion,
                                                    ));
                                                }
                                            }
                                            if let Some(payload) =
                                                response.dnd_release_payload::<ViewDatabaseDrag>()
                                                && payload.view != view_index
                                            {
                                                merge_from_view =
                                                    Some((payload.view, view_index));
                                            }
                                            if response.clicked() {
                                                preview = Some((view_index, id));
                                            }
                                        }
                                    });
                            }
                            ui.separator();
                        }
                    });
            });

        self.views_open = open;
        if open_request && let Some(path) = open_dialog(self.last_database_dir.as_deref()) {
            self.open_database_view(&path);
        }
        if let Some(index) = close_view
            && index < self.views.len()
        {
            let view = self.views.remove(index);
            self.status = format!("Closed database view {}.", view.name());
        }
        if let Some((view_index, id, insertion)) = reorder_view
            && let Some(view) = self.views.get_mut(view_index)
        {
            view.session.move_slot(id, insertion);
        }
        if let Some((source, game_id, target, insert_at)) = drop_from_view
            && let Some(game) = self.views.get(source).and_then(|view| {
                view.session
                    .load(game_id, &std::collections::HashMap::new())
                    .ok()
            })
        {
            self.pending_game_drop = Some(PendingGameDrop {
                label: format!(
                    "Game from {}",
                    self.views
                        .get(source)
                        .map(DatabaseView::name)
                        .unwrap_or_else(|| "database view".to_string())
                ),
                game,
                target: DropTarget::View(target),
                insert_at,
            });
        }
        if let Some((id, target, insert_at)) = drop_from_main
            && let Ok(game) = self.session.load(id, &self.edited)
        {
            self.pending_game_drop = Some(PendingGameDrop {
                game,
                target: DropTarget::View(target),
                insert_at,
                label: "Game from the current list".to_string(),
            });
        }
        if let Some(view_index) = merge_to_main
            && let Some(view) = self.views.get(view_index)
        {
            self.pending_merge = Some(PendingMerge {
                library: view.library.clone(),
                target: DropTarget::Main,
                label: view.name(),
            });
        }
        if let Some((source, target)) = merge_from_view
            && let Some(view) = self.views.get(source)
        {
            self.pending_merge = Some(PendingMerge {
                library: view.library.clone(),
                target: DropTarget::View(target),
                label: view.name(),
            });
        }
        if let Some((view_index, id)) = preview
            && let Some(game) = self.views.get(view_index).and_then(|view| {
                view.session
                    .load(id, &std::collections::HashMap::new())
                    .ok()
            })
        {
            let label = format!(
                "{} · {} vs {}",
                self.views[view_index].name(),
                game.white(),
                game.black()
            );
            self.open_game_preview(game, label);
        }
    }

    fn pending_game_drop_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_game_drop.take() else {
            return;
        };
        let mut window_open = true;
        let mut keep = true;
        let mut action = None;
        egui::Window::new("Drop game")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(&pending.label).strong());
                ui.label(format!(
                    "{} – {}",
                    pending.game.white(),
                    pending.game.black()
                ));
                let target = match pending.target {
                    DropTarget::Main => "the current list".to_string(),
                    DropTarget::View(index) => self
                        .views
                        .get(index)
                        .map(|view| view.name())
                        .unwrap_or_else(|| "the database view".to_string()),
                };
                ui.label(format!(
                    "Drop it at position {} in {target}, or append it to the end?",
                    pending.insert_at + 1
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Insert here").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Append").clicked() {
                        action = Some(false);
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        if let Some(insert) = action {
            self.finish_game_drop(pending, insert);
        } else if window_open && keep {
            self.pending_game_drop = Some(pending);
        }
    }

    fn pending_merge_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_merge.take() else {
            return;
        };
        let mut window_open = true;
        let mut keep = true;
        let mut action = false;
        let target = match pending.target {
            DropTarget::Main => "the current list".to_string(),
            DropTarget::View(index) => self
                .views
                .get(index)
                .map(|view| view.name())
                .unwrap_or_else(|| "the database view".to_string()),
        };
        let count = pending.library.len();
        egui::Window::new("Merge database")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Merge {} from {} into {target}?",
                    games_label(count),
                    pending.label
                ));
                ui.label(
                    RichText::new(
                        "Games with matching players, date, result, and opening moves are skipped as duplicates. The source database is never modified.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Merge").clicked() {
                        action = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        if action {
            self.finish_merge(pending);
        } else if window_open && keep {
            self.pending_merge = Some(pending);
        }
    }

    fn pending_open_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_open_choice.take() else {
            return;
        };
        let mut window_open = true;
        let mut keep = true;
        let mut action = None;
        egui::Window::new("Database already open")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "A database is already open. What should Brett do with {}?",
                    pending.path.display()
                ));
                ui.label(
                    RichText::new(
                        "Merge skips duplicates; opening a view keeps the current database on the board.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open (replace)").clicked() {
                        action = Some(0);
                    }
                    if ui.button("Merge into current").clicked() {
                        action = Some(1);
                    }
                    if ui.button("Open in database views").clicked() {
                        action = Some(2);
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        match action {
            Some(0) => self.load_file(&pending.path),
            Some(1) => match open_library(&pending.path) {
                Ok(library) => {
                    self.pending_merge = Some(PendingMerge {
                        library,
                        target: DropTarget::Main,
                        label: pending.path.display().to_string(),
                    });
                }
                Err(err) => {
                    self.status = format!("Could not read {}: {err}", pending.path.display());
                }
            },
            Some(2) => self.open_database_view(&pending.path),
            _ => {
                if window_open && keep {
                    self.pending_open_choice = Some(pending);
                }
            }
        }
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
                            ui.selectable_value(
                                &mut self.online_source,
                                OnlineSource::ChessAbc,
                                "ChessABC",
                            );
                        });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.online_username)
                        .hint_text(self.online_source.input_hint())
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
                        "Lichess exports in one request. Chess.com is assembled from recent monthly archives; at most 36 months are read. ChessABC searches by player name or FIDE id and pages through its latest games.",
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

    fn index_confirmation_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_large_index {
            return;
        }
        let count = self
            .library
            .as_ref()
            .map(|library| library.len())
            .unwrap_or(0);
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Build full position index")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("This database contains {}.", games_label(count)));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "A full position index can take several minutes and multiple gigabytes of RAM. The app may become unresponsive while it is being built.",
                    )
                    .strong(),
                );
                ui.label(
                    RichText::new(
                        "For multi-million-game archives, use a smaller opening-tree or reference database. Full indexing is disabled above 1,000,000 games until the external-sort indexer is available.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Build index").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.confirm_large_index = false;
            self.start_index();
        } else if cancel || !open {
            self.confirm_large_index = false;
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let default_correspondence = default_correspondence_path();
        let config_path = crate::settings::cloud_path();
        let mut open = self.show_settings;
        let mut save = false;
        let mut close = false;
        let mut choose_correspondence = false;
        let mut use_default_correspondence = false;
        let mut choose_engine_idx = None;

        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.heading("Player ratings");
                ui.label(
                    RichText::new(
                        "Priority orders the available tags. When several ratings exist, you can show all of them instead of only the preferred one.",
                    )
                    .small()
                    .weak(),
                );
                ui.horizontal_wrapped(|ui| {
                    for priority in [
                        RatingPriority::Elo,
                        RatingPriority::Dwz,
                        RatingPriority::Uscf,
                    ] {
                        ui.radio_value(
                            &mut self.settings_rating_priority,
                            priority,
                            priority.label(),
                        );
                    }
                });
                ui.checkbox(
                    &mut self.settings_show_all_ratings,
                    "Show all available ratings",
                )
                .on_hover_text(
                    "Display Elo, DWZ, and USCF together when a game carries more than one rating.",
                );

                ui.add_space(8.0);
                ui.separator();
                ui.heading("View segments");
                ui.label(
                    RichText::new("Choose which interface elements to display in the workspace.")
                        .small()
                        .weak(),
                );
                ui.checkbox(&mut self.settings_show_pgn_tags, "Show editable PGN tags");
                ui.checkbox(
                    &mut self.settings_show_annotations,
                    "Show move annotations and comment editor",
                );
                ui.checkbox(
                    &mut self.settings_online_reference,
                    "Show Lichess / online reference side panel",
                );
                ui.checkbox(
                    &mut self.settings_show_player_photos,
                    "Show player portraits",
                );
                ui.checkbox(
                    &mut self.settings_show_clocks,
                    "Show PGN clocks in the board strips",
                );
                ui.checkbox(
                    &mut self.settings_engine_bar,
                    "Show the evaluation bar beside the board",
                );
                ui.checkbox(
                    &mut self.settings_show_nav_buttons,
                    "Show move navigation buttons",
                );
                ui.checkbox(
                    &mut self.settings_show_empty_pgn_tags,
                    "Show absent or empty PGN tags",
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Photo size");
                    for size in PhotoSize::ALL {
                        ui.radio_value(&mut self.settings_photo_size, size, size.label());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Champion lineage");
                    for lineage in ChampionLineage::ALL {
                        ui.radio_value(
                            &mut self.settings_champion_lineage,
                            lineage,
                            lineage.label(),
                        );
                    }
                });
                ui.label(
                    RichText::new(
                        "Photo size affects the board strips and the selected-player header; the Players list keeps a fixed thumbnail. FIDE/inclusive adds the four FIDE knockout champions from 1999–2006.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Move trends");
                ui.label(
                    RichText::new(
                        "The Tree tab compares a recent window with a historical baseline. Small samples are shrunk toward the baseline and never shown as a trend.",
                    )
                    .small()
                    .weak(),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Recent window");
                    for window in crate::recent::TrendWindow::ALL {
                        ui.radio_value(&mut self.settings_trend_window, window, window.label());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Baseline");
                    for baseline in crate::recent::TrendBaseline::ALL {
                        ui.radio_value(&mut self.settings_trend_baseline, baseline, baseline.label());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Minimum games per window").small().strong());
                    ui.add(
                        egui::DragValue::new(&mut self.settings_trend_min_games)
                            .range(3..=200)
                            .speed(1),
                    );
                });

                ui.add_space(8.0);
                ui.separator();
                ui.heading("My player names");
                ui.label(
                    RichText::new(
                        "One name per line, highest priority first. The first name replaces “You” in preparation tags. When a game contains one of these names, the board opens from that side; the first matching name wins.",
                    )
                    .small()
                    .weak(),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.settings_my_player_names)
                        .desired_rows(3)
                        .hint_text("Lakis01\nJohn Smith"),
                );
                ui.label(
                    RichText::new(
                        "The first name is labelled “You” in the Players tab. Names are matched without regard to word order or punctuation.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Game review");
                ui.checkbox(
                    &mut self.settings_review_skip_book,
                    "Skip opening-book moves during game review",
                )
                .on_hover_text(
                    "Checks the loaded native book, local opening tree, and Lichess Masters explorer before running the engine.",
                );
                ui.add_enabled_ui(self.settings_review_skip_book, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Min Masters games").small().strong());
                        ui.add(
                            egui::DragValue::new(&mut self.settings_review_book_min_games)
                                .range(1..=100)
                                .speed(1),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new("Max book plies").small().strong());
                        ui.add(
                            egui::DragValue::new(&mut self.settings_review_book_max_plies)
                                .range(4..=80)
                                .speed(1),
                        );
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Correspondence database");
                ui.checkbox(
                    &mut self.settings_correspondence_enabled,
                    "Include the correspondence database in the Engine tab",
                )
                .on_hover_text(
                    "The database is used as an automatic position reference. It is read-only.",
                );
                ui.add_enabled_ui(self.settings_correspondence_enabled, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("File").small().strong());
                        let width = (ui.available_width() - 176.0).max(120.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_correspondence_path)
                                .desired_width(width)
                                .hint_text("e.g. Correspondence_2350.si4"),
                        );
                        if ui.button("Choose…").clicked() {
                            choose_correspondence = true;
                        }
                        if ui
                            .add_enabled(
                                default_correspondence.is_some(),
                                egui::Button::new("Use default"),
                            )
                            .on_hover_text("Use the conventional Brett Resources location")
                            .clicked()
                        {
                            use_default_correspondence = true;
                        }
                    });
                    ui.label(RichText::new(&self.correspondence_status).small().weak());
                });
                if let Some(path) = &default_correspondence {
                    ui.label(
                        RichText::new(format!("Default location: {}", path.display()))
                            .small()
                            .weak(),
                    );
                } else {
                    ui.label(
                        RichText::new(
                            "No default correspondence database was found; choose a file.",
                        )
                        .small()
                        .weak(),
                    );
                }
                ui.label(
                    RichText::new(
                        "Brett builds a cached position index once; later starts reuse it.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Lichess API");
                ui.label(
                    RichText::new(
                        "The opening explorer and online-account lookups use a personal Lichess API token. No scopes are required. The public cloud evaluation works without a token.",
                    )
                    .small()
                    .weak(),
                );
                ui.hyperlink_to(
                    "Create a personal Lichess API token",
                    "https://lichess.org/account/oauth/token",
                );
                if self.settings_token_from_env {
                    ui.label(
                        RichText::new(
                            "BRETT_LICHESS_TOKEN is set and takes precedence over the saved token.",
                        )
                        .small()
                        .color(GOLD),
                    );
                } else {
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 70.0).max(120.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_token)
                                .desired_width(width)
                                .password(!self.settings_token_visible)
                                .hint_text("lip_…"),
                        );
                        ui.checkbox(&mut self.settings_token_visible, "Show");
                    });
                    ui.label(
                        RichText::new(
                            "The token is stored in the references config file. Set BRETT_LICHESS_TOKEN to keep it out of the file.",
                        )
                        .small()
                        .weak(),
                    );
                }
                if let Some(path) = &config_path {
                    ui.label(
                        RichText::new(format!("Config file: {}", path.display()))
                            .small()
                            .weak(),
                    );
                }

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Player profiles");
                ui.checkbox(
                    &mut self.settings_wiki_enabled,
                    "Load Wikipedia summaries when a player profile opens",
                )
                .on_hover_text(
                    "Only the selected player is looked up. Opening a database or building the player list never contacts Wikipedia.",
                );
                ui.label(
                    RichText::new(
                        "Summaries come from Wikipedia, are cached locally, and are shown with CC BY-SA attribution. Brett does not bundle Wikipedia content.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Analysis engines & hotkeys (1, 2, 3)");
                ui.label(
                    RichText::new(
                        "Configure up to 3 analysis engines. Press hotkeys 1, 2, or 3 (or Alt+1, 2, 3) to toggle each engine on/off at any time during analysis.",
                    )
                    .small()
                    .weak(),
                );
                for idx in 0..3 {
                    ui.group(|ui| {
                        let name = self.engine_slots[idx].short_name(idx);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Engine slot {} [Hotkey {}]: {}", idx + 1, idx + 1, name)).strong());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.checkbox(&mut self.engine_slots[idx].enabled, "Active");
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Executable");
                            let width = (ui.available_width() - 170.0).max(120.0);
                            let hint = if idx == 0 {
                                "/usr/bin/stockfish"
                            } else {
                                "Path to engine executable"
                            };
                            ui.add(
                                egui::TextEdit::singleline(&mut self.engine_slots[idx].path)
                                    .desired_width(width)
                                    .hint_text(hint),
                            );
                            if ui.button("Browse…").clicked() {
                                choose_engine_idx = Some(idx);
                            }
                            if idx == 0 && ui.button("Default").on_hover_text("Reset to default Stockfish").clicked() {
                                self.engine_slots[0].path = crate::engine::default_engine_path()
                                    .display()
                                    .to_string();
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Threads");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_slots[idx].threads)
                                    .range(1..=256)
                                    .speed(1),
                            );
                            ui.add_space(8.0);
                            ui.label("Hash / RAM");
                            ui.add(
                                egui::DragValue::new(&mut self.engine_slots[idx].hash_mb)
                                    .range(1..=32_768)
                                    .suffix(" MB")
                                    .speed(64),
                            );
                        });
                    });
                    ui.add_space(3.0);
                }

                if !self.settings_status.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(&self.settings_status)
                            .small()
                            .color(Color32::from_rgb(0xE0, 0x8A, 0x7A)),
                    );
                }

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(idx) = choose_engine_idx && let Some(path) = open_engine_dialog() {
            self.engine_slots[idx].path = path.display().to_string();
            if idx == 0 {
                self.engine_path = path.display().to_string();
            }
        }
        if choose_correspondence && let Some(path) = open_dialog(None) {
            self.settings_correspondence_path = path.display().to_string();
            self.settings_correspondence_enabled = true;
        }
        if use_default_correspondence && let Some(path) = default_correspondence {
            self.settings_correspondence_path = path.display().to_string();
            self.settings_correspondence_enabled = true;
        }
        if save && self.apply_settings() {
            self.show_settings = false;
        }
        if close || !open {
            self.show_settings = false;
        }
    }

    /// Standalone, larger view of a player portrait. The source pack stores
    /// 80×100 images, so 240×300 is a deliberate 3× enlargement rather than a
    /// claim of extra detail.
    fn photo_viewer_window(&mut self, ctx: &egui::Context) {
        let Some(name) = self.photo_viewer.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Player photo")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(264.0)
            .show(ctx, |ui| {
                if let Some(texture) = self.photo_texture(ctx, &name) {
                    ui.add(
                        egui::Image::from_texture(&texture).fit_to_exact_size(vec2(240.0, 300.0)),
                    );
                } else {
                    ui.label(
                        RichText::new("No photo is available for this player.")
                            .weak()
                            .italics(),
                    );
                }
                ui.add_space(4.0);
                ui.label(RichText::new(&name).strong());
                if let Some(champion) = champions::champion(&name, self.champion_lineage) {
                    ui.label(RichText::new("👑 World champion").color(GOLD))
                        .on_hover_text(champion.tooltip());
                }
            });
        if !open {
            self.photo_viewer = None;
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
                ui.label("Rules and SAN: cozy-chess. PGN: Brett's own streaming parser.");
                ui.label(
                    "SCID, classic CBH, and 2CBH readers: independent native implementations.",
                );
                ui.label(
                    "Player photos: optional Scid Player_photos pack derived from \
                     scid/PlayerData/Player_photos.zip; not part of Brett's MIT source \
                     and not redistributed with Brett.",
                );
                ui.label("Chess symbols: DejaVu Sans, Bitstream Vera / public-domain licence.");
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    self.show_about = false;
                }
            });
    }

    fn cache_info_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_cache_info {
            return;
        }
        let mut close_dialog = false;
        egui::Window::new("Local Analysis Cache & Storage")
            .open(&mut self.show_cache_info)
            .collapsible(false)
            .resizable(true)
            .default_width(500.0)
            .show(ctx, |ui| {
                ui.heading("Storage Footprint & Longevity");
                ui.label(
                    "Local engine evaluations are stored in an embedded SQLite database using \
                     Write-Ahead Logging (WAL). Each analyzed position and principal variation \
                     consumes ~180 bytes on disk.",
                );
                ui.add_space(6.0);
                egui::Grid::new("cache_storage_grid")
                    .striped(true)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Analyzed Positions").strong());
                        ui.label(RichText::new("Estimated Disk Space").strong());
                        ui.end_row();

                        ui.label("1,000 positions");
                        ui.label("~180 KB");
                        ui.end_row();

                        ui.label("10,000 positions");
                        ui.label("~1.8 MB");
                        ui.end_row();

                        ui.label("100,000 positions");
                        ui.label("~18 MB");
                        ui.end_row();

                        ui.label("1,000,000 positions");
                        ui.label("~180 MB");
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(
                    "Even over decades of heavy daily preparation, the cache takes at most \
                     a few hundred megabytes, which is negligible on modern storage.",
                );
                ui.separator();
                ui.heading("Intelligent Caching & Coexistence");
                ui.label(
                    "• Debounced writes: early plies commit every 10 seconds; deep plies (depth ≥ 20) commit immediately.\n\
                     • Smart Stockfish replacement: deeper analyses supersede earlier/weaker ones based on search depth and calibrated version ratings.\n\
                     • Coexistence: Leela Chess Zero evaluations are partitioned separately and never overwritten by Stockfish.\n\
                     • Zero network overhead: the replacement heuristic operates 100% in-memory locally with 0 bytes bandwidth.",
                );
                ui.separator();
                ui.heading("Seamless Migration Across Machines");
                ui.label(
                    "You can easily move your analysis and settings to another machine or OS:\n\
                     • Cache Migration: In the Engine drawer, click 'Export cache…' to export a clean, compacted .db snapshot (via SQLite VACUUM INTO), or 'Import cache…' to merge an existing database.\n\
                     • Settings Migration: In the top Settings menu, click 'Export all settings…' or 'Import all settings…' to transfer all configuration and preparation profiles as a single JSON file.",
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Close").clicked() {
                        close_dialog = true;
                    }
                });
            });
        if close_dialog {
            self.show_cache_info = false;
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // `logic` also runs while the window is minimized, so a close request
        // cannot slip through without the unsaved-changes prompt.
        self.handle_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_open();
        self.poll_index();
        self.poll_tree_index();
        self.poll_player_directory();
        self.poll_book();
        self.poll_player_book();
        self.poll_wiki();
        self.poll_reference();
        self.poll_correspondence();
        self.poll_online();
        self.poll_online_game();
        self.poll_online_reference();
        self.poll_prep_online();
        self.poll_cloud();
        self.poll_bar_cloud();
        self.poll_explorer();
        self.poll_review_book();
        self.poll_engine();
        self.poll_review();
        self.maybe_engine_analysis();
        self.maybe_request_bar_cloud();
        if self.open_rx.is_some()
            || self.index_rx.is_some()
            || self.tree_index_rx.is_some()
            || self.players_rx.is_some()
            || self.book_rx.is_some()
            || self.player_book_rx.is_some()
            || self.reference_rx.is_some()
            || self.online_rx.is_some()
            || self.online_game_rx.is_some()
            || self.online_ref_rx.is_some()
            || self.cloud_rx.is_some()
            || self.bar_cloud_rx.is_some()
            || self.explorer_rx.is_some()
            || self.review_book_rx.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.engine_searching || self.engine_slots.iter().any(|s| s.engine.is_some() && !s.ready) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.review.as_ref().is_some_and(|review| review.running) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
      
... [truncated for diff preview]