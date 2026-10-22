use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    pos2, vec2,
};
use shakmaty::fen::Fen;
use shakmaty::san::SanPlus;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, EnPassantMode, Move, Position, Square};

use crate::domain::{self, Clock, Game, Token};
use crate::engine::{EngineEvent, EngineHandle, EngineInfo, Score, SearchLimit};
use crate::library::{GameLibrary, PgnLibrary};
use crate::online::{self, ImportResult, OnlineSource};
use crate::players::{self, PlayerBook, PlayerDirectory};
use crate::position::{self, BuildOptions, BuildProgress, PositionIndex, PositionStats};
use crate::scid::ScidLibrary;
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
}

enum IndexMessage {
    Progress(BuildProgress),
    Done(Result<PositionIndex, String>),
}

enum PlayerBookMessage {
    Progress { done: usize, total: usize },
    Done(Result<PlayerBook, String>),
}

enum OnlineMessage {
    Progress(String),
    Done(Result<ImportResult, String>),
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
            status: "Open a PGN or SCID database, or drop one onto the window.".to_string(),
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
            annotation_editor: None,
            annotation_focus: false,
            online_open: false,
            online_source: OnlineSource::Lichess,
            online_username: String::new(),
            online_max: 500,
            online_status: String::new(),
            online_rx: None,
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
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let opened: anyhow::Result<Arc<dyn GameLibrary>> =
            if extension == "si4" || extension == "si3" {
                ScidLibrary::open(path).map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
            } else {
                PgnLibrary::build(path.to_path_buf())
                    .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
            };
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

    /// Start the engine lazily. The default is Stockfish, overridable through
    /// `BRETT_ENGINE`.
    fn ensure_engine(&mut self) -> bool {
        if self.engine.is_some() {
            return true;
        }
        let path = std::env::var_os("BRETT_ENGINE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/stockfish"));
        match EngineHandle::spawn(&path) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.engine_ready = false;
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

    /// Load one game tree from the index, replacing the current game.
    fn select_game(&mut self, index: usize) {
        if index == self.game_idx && self.current.is_some() && self.prep.is_none() {
            return;
        }
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
            self.drag_from = None;
            self.selected = None;
            self.targets.clear();
            self.annotation_editor = None;
            self.annotation_focus = false;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (right, left, down, up, home, end, flip, tree, moves, annotate, engine) =
            ctx.input(|i| {
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
        if annotate && self.current.is_some() {
            self.right_tab = RightTab::Moves;
            self.annotation_editor = Some(self.node);
            self.annotation_focus = true;
        }
        if engine {
            self.right_tab = RightTab::Engine;
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
                    ui.label("Or drop a .pgn, .si4, or .si3 onto the window.");
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
                });
                ui.separator();
                match self.right_tab {
                    RightTab::Moves => self.moves_content(ui),
                    RightTab::Tree => self.tree_content(ui),
                    RightTab::Engine => self.engine_content(ui),
                }
            });
    }

    fn engine_content(&mut self, ui: &mut Ui) {
        self.maybe_engine_analysis();

        let mut start = false;
        let mut stop = false;
        let mut settings_changed = false;
        let mut clicked_uci = None;

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
                        "Stockfish / UCI"
                    } else {
                        &self.engine_name
                    })
                    .small()
                    .weak(),
                )
                .on_hover_text("Set BRETT_ENGINE to use a different UCI engine");
                ui.label(RichText::new(&self.engine_status).small().weak());
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
        if let Some(prep) = self.prep.clone() {
            self.prep_tree(ui, &prep, &pos, key);
            return;
        }
        let stats = self.index.as_ref().and_then(|index| index.stats(key));
        let moves = self
            .index
            .as_ref()
            .map(|index| index.moves(key))
            .unwrap_or_default();

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

        ui.add_space(4.0);
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
            ui.label(RichText::new("No indexed game reached this position.").weak());
        }

        if moves.is_empty() {
            return;
        }

        let mut rows: Vec<(String, Move, position::MoveStat)> = moves
            .into_iter()
            .filter_map(|stat| {
                let mv = stat.to_move(&pos)?;
                let san = stat.san(&pos)?;
                Some((san, mv, stat))
            })
            .collect();
        rows.sort_by(|a, b| b.2.games.cmp(&a.2.games).then(a.0.cmp(&b.0)));

        let mut clicked = None;
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("opening-tree")
            .max_height((ui.available_height() - 8.0).max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
            });

        if let Some(mv) = clicked {
            self.play_move(mv);
        }
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

        if pos.turn() != prep.opponent_color() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "It is your move. Play a move; {}'s likely replies appear here.",
                    prep.player_name
                ))
                .italics(),
            );
            return;
        }

        let (prediction, stats) = {
            let Some(book) = self
                .player_book
                .as_ref()
                .filter(|book| book.player_key() == prep.player_key)
            else {
                ui.add_space(8.0);
                ui.label(RichText::new(&self.player_book_status).weak());
                if self.player_book_job.as_deref() == Some(prep.player_key.as_str()) {
                    ui.spinner();
                }
                return;
            };
            let global = self
                .index
                .as_ref()
                .map(|index| index.moves(key))
                .unwrap_or_default();
            (players::predict(book, key, &global), book.stats(key))
        };

        if let Some(stats) = stats {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} player games here",
                        games_label(stats.games as usize)
                    ))
                    .strong(),
                );
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
        ui.label(
            RichText::new(format!(
                "Recency-weighted Bayes · {} player games · {} in the last 2 years · prior from the database",
                prediction.player_games, prediction.recent_games
            ))
            .weak()
            .small(),
        )
        .on_hover_text(
            "The model applies exponential time decay (18-month half-life), a small Dirichlet prior from database move frequencies, and a two-year recent-window mixture. It is a probability estimate, not a guarantee.",
        );

        if prediction.moves.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("No move observations to predict from.").weak());
            return;
        }

        let top = prediction.moves[0];
        if let Some(mv) = position::decode_move(pos, top.mv) {
            let san = shakmaty::san::SanPlus::from_move(pos.clone(), mv).to_string();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Predicted:").strong());
                ui.label(
                    RichText::new(domain::figurine_san(&san))
                        .monospace()
                        .size(18.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", top.probability * 100.0))
                        .strong()
                        .color(Color32::from_rgb(0x9F, 0xD0, 0xA8)),
                );
                if top.player_games == 0 {
                    ui.label(RichText::new("database prior only").weak().small());
                }
            });
        }

        let mut clicked = None;
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("player-tree")
            .max_height((ui.available_height() - 8.0).max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("player-tree-grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([10.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Move").strong());
                        ui.label(RichText::new("Likelihood").strong());
                        ui.label(RichText::new("Games").strong());
                        ui.label(RichText::new("Recent").strong());
                        ui.label(RichText::new("Last").strong());
                        ui.label(RichText::new("Trend").strong());
                        ui.end_row();

                        for predicted in prediction.moves.iter().take(24) {
                            let Some(mv) = position::decode_move(pos, predicted.mv) else {
                                continue;
                            };
                            let san =
                                shakmaty::san::SanPlus::from_move(pos.clone(), mv).to_string();
                            let figurine = domain::figurine_san(&san);
                            let mut text = RichText::new(figurine).monospace();
                            if predicted.player_games == 0 {
                                text = text.weak();
                            }
                            if ui
                                .selectable_label(false, text)
                                .on_hover_text("Play this response")
                                .clicked()
                            {
                                clicked = Some(mv);
                            }
                            probability_bar(ui, predicted.probability);
                            ui.label(predicted.player_games.to_string());
                            ui.label(predicted.recent_games.to_string());
                            ui.label(short_date(predicted.last_date));
                            let (trend, color) = trend_label(predicted.trend_pp);
                            ui.label(RichText::new(trend).small().color(color));
                            ui.end_row();
                        }
                    });
            });

        ui.label(
            RichText::new("Probabilities cover moves seen in this player's or the database's games at this position.")
                .weak()
                .small(),
        );
        if let Some(mv) = clicked {
            self.play_move(mv);
        }
    }

    fn play_move(&mut self, mv: Move) {
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
                            .hint_text("/path/to/file.pgn or database.si4")
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
                ui.label("SCID reader: independent native implementation.");
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
        self.poll_online();
        self.poll_engine();
        self.maybe_engine_analysis();
        if self.index_rx.is_some() || self.player_book_rx.is_some() || self.online_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.engine_searching || (self.engine.is_some() && !self.engine_ready) {
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
                        RichText::new("PGN, SCID4, or SCID3 · or use File → Open database…").weak(),
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
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(elo.to_string());
    }
    game.header(dwz_key)
        .map(str::trim)
        .filter(|v| !v.is_empty())
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

/// Horizontal probability bar used by the player-prediction table.
fn probability_bar(ui: &mut Ui, probability: f64) {
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
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{:.0}%", probability * 100.0),
        FontId::proportional(10.0),
        Color32::WHITE,
    );
    response.on_hover_text(format!(
        "Estimated probability of this move in the next game: {:.1}%",
        probability * 100.0
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
                "--file-filter=Chess databases | *.pgn *.PGN *.si4 *.si3",
                "--file-filter=PGN files | *.pgn *.PGN",
                "--file-filter=SCID databases | *.si4 *.si3",
                "--file-filter=All files | *",
            ],
        ),
        (
            "kdialog",
            &[
                "--getopenfilename",
                ".",
                "*.pgn *.PGN *.si4 *.si3|Chess databases",
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
            annotation_editor: None,
            annotation_focus: false,
            online_open: false,
            online_source: OnlineSource::Lichess,
            online_username: String::new(),
            online_max: 500,
            online_status: String::new(),
            online_rx: None,
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
}
