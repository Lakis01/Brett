use eframe::egui::{self, Align, Align2, Color32, FontId, Key, Layout, RichText, Sense, Ui, vec2};
use shakmaty::{Color, Position, Square};

use crate::domain::{self, Game, Token};
use crate::library::PgnLibrary;
use crate::ui::board::{self, BoardState};
use crate::ui::pieces::PieceTextures;

pub struct App {
    library: Option<PgnLibrary>,
    current: Option<Game>,
    /// Games edited during this session, kept by index so switching away and
    /// back does not discard new moves or variations.
    edited: std::collections::HashMap<usize, Game>,
    dirty: bool,
    game_idx: usize,
    node: usize,
    orientation: Color,
    selected: Option<Square>,
    targets: Vec<Square>,
    status: String,
    index_status: String,
    path_input: String,
    filter: String,
    show_about: bool,
    textures: PieceTextures,
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

        let mut app = Self {
            library: None,
            current: None,
            edited: std::collections::HashMap::new(),
            dirty: false,
            game_idx: 0,
            node: 0,
            orientation: Color::White,
            selected: None,
            targets: Vec::new(),
            status: "Open a PGN file, or drop one onto the window.".to_string(),
            index_status: String::new(),
            path_input: String::new(),
            filter: String::new(),
            show_about: false,
            textures: PieceTextures::load(&cc.egui_ctx),
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
        self.node = node;
        self.selected = None;
        self.targets.clear();
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
        match PgnLibrary::build(path.to_path_buf()) {
            Ok(library) => {
                let count = library.len();
                let indexed_in = library.indexed_in();
                self.library = Some(library);
                self.edited.clear();
                self.dirty = false;
                self.game_idx = 0;
                self.current = None;
                self.goto(0);
                self.index_status = format!(
                    "{} from {} · indexed in {:.2}s",
                    games_label(count),
                    path.display(),
                    indexed_in.as_secs_f64()
                );
                self.status = self.index_status.clone();
                self.path_input = path.display().to_string();
                self.select_game(0);
            }
            Err(err) => self.status = format!("Could not read {}: {err}", path.display()),
        }
    }

    /// Load one game tree from the index, replacing the current game.
    fn select_game(&mut self, index: usize) {
        if index == self.game_idx && self.current.is_some() {
            return;
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
        let Some(pos) = self
            .current
            .as_ref()
            .map(|game| game.nodes[self.node].pos.clone())
        else {
            return;
        };

        if let Some(from) = self.selected
            && from != sq
            && let Some(mv) = domain::pick_move(&pos, from, sq)
        {
            let Some(game) = self.current.as_mut() else {
                return;
            };
            let id = game.add_move(self.node, mv);
            self.dirty = true;
            self.goto(id);
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

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (right, left, down, up, home, end, flip) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::F),
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
    }

    fn menu_bar(&mut self, root: &mut Ui) {
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open PGN…").clicked() {
                        if let Some(path) = open_dialog() {
                            self.load_file(&path);
                        }
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Or drop a .pgn onto the window.");
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

    fn games_panel(&mut self, root: &mut Ui) {
        egui::Panel::left("games")
            .default_size(300.0)
            .size_range(220.0..=460.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                let count = self.library.as_ref().map(|l| l.len()).unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.heading("Games");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(count.to_string()).weak());
                    });
                });
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
                egui::ScrollArea::vertical()
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
                            let selected = idx == self.game_idx;
                            let width = ui.available_width();
                            if game_row(ui, selected, &title, &subtitle, width, row_h - 2.0) {
                                self.select_game(idx);
                            }
                        }
                    });
            });
    }

    fn moves_panel(&mut self, root: &mut Ui) {
        egui::Panel::right("moves")
            .default_size(340.0)
            .size_range(260.0..=520.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                let current_node = self.node;
                let data = self.game().map(|game| {
                    let node = current_node.min(game.nodes.len().saturating_sub(1));
                    (
                        format!("{} – {}", game.white(), game.black()),
                        format!("{} · {} · {}", game.event(), game.date(), game.result()),
                        domain::tokens(game),
                        game.nodes[node].comment.clone(),
                        fen_string(&game.nodes[node].pos),
                    )
                });

                let Some((title, subtitle, tokens, comment, fen)) = data else {
                    ui.heading("No game");
                    return;
                };

                ui.heading(title);
                ui.label(RichText::new(subtitle).weak());
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("moves")
                    .max_height(ui.available_height() * 0.58)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for token in &tokens {
                                match token {
                                    Token::OpenParen => {
                                        ui.label(RichText::new("(").weak());
                                    }
                                    Token::CloseParen => {
                                        ui.label(RichText::new(")").weak());
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
                                            ui.label(RichText::new(prefix).weak().small());
                                        }
                                        if ui
                                            .selectable_label(
                                                *node == self.node,
                                                RichText::new(san).monospace(),
                                            )
                                            .clicked()
                                        {
                                            self.goto(*node);
                                        }
                                    }
                                }
                            }
                        });
                    });

                ui.separator();
                ui.label(RichText::new("Comment").strong());
                if comment.is_empty() {
                    ui.label(RichText::new("—").weak());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("comment")
                        .max_height(120.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label(comment);
                        });
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FEN").strong());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(fen.clone());
                    }
                });
                let mut fen_value = fen;
                ui.add(
                    egui::TextEdit::singleline(&mut fen_value)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .interactive(false),
                );
            });
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
                            .hint_text("/path/to/file.pgn")
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
        self.games_panel(ui);
        self.moves_panel(ui);
        self.bottom_bar(ui);

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let avail = ui.available_size();
            let side = (avail.x.min(avail.y) - 24.0).max(120.0);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space(8.0);
                let board_data = self.game().map(|game| {
                    let node = &game.nodes[self.node];
                    (&node.pos, domain::last_move_squares(node))
                });
                if let Some((pos, last_move)) = board_data {
                    let clicked = board::draw(
                        ui,
                        &self.textures,
                        BoardState {
                            pos,
                            orientation: self.orientation,
                            last_move,
                            selected: self.selected,
                            targets: &self.targets,
                        },
                        side,
                    );
                    if let Some(sq) = clicked {
                        self.click_square(sq);
                    }
                } else {
                    ui.add_space(40.0);
                    ui.label(RichText::new("Drop a PGN file here").heading());
                    ui.label(RichText::new("or use File → Open PGN…").weak());
                }
            });
        });

        self.about(&ctx);
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

/// Native file dialog. Prefers zenity/kdialog (both common on Linux desktops)
/// and returns `None` when no dialog is available or the user cancels.
fn open_dialog() -> Option<std::path::PathBuf> {
    use std::process::Command;

    let dialogs: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Open PGN",
                "--file-filter=PGN files | *.pgn *.PGN",
                "--file-filter=All files | *",
            ],
        ),
        (
            "kdialog",
            &["--getopenfilename", ".", "*.pgn *.PGN|PGN files"],
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
