//! Board position setup dialog and validation.
//!
//! Lets the user start a game from an arbitrary legal position without
//! hand-editing a PGN `FEN` tag. Supports piece placement, removal, moving,
//! side to move, castling rights, en passant square, clocks, standard chess
//! and Chess960, with live legality validation and FEN import/export.

use std::num::NonZeroU32;

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2};

use crate::chess::{
    Bitboard, Board, Castles, CastlingMode, CastlingSide, Chess, Color, EnPassantMode, Fen, File,
    Piece, Rank, Role, Setup, Square,
};
use crate::domain::{Game, Node};
use crate::ui::board::{DARK, LIGHT, SELECTED, is_dark, square_at, square_at_pos};
use crate::ui::pieces::PieceTextures;

const GOLD: Color32 = Color32::from_rgb(0xD8, 0xA2, 0x30);
const GREEN: Color32 = Color32::from_rgb(0x4E, 0x9A, 0x4E);
const RED: Color32 = Color32::from_rgb(0xD8, 0x4A, 0x4A);

/// Tool currently active in the position editor palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupTool {
    /// Select and move pieces on the board.
    Pointer,
    /// Place this piece on click or drag.
    Piece(Piece),
    /// Erase the piece on click.
    Eraser,
}

/// State for the board setup dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionSetupState {
    pub setup: Setup,
    pub mode: CastlingMode,
    pub orientation: Color,
    pub tool: SetupTool,
    pub selected_square: Option<Square>,
    pub drag_from_square: Option<Square>,
    pub fen_input: String,
    pub fen_error: Option<String>,
    pub validation_error: Option<String>,
}

impl Default for PositionSetupState {
    fn default() -> Self {
        let setup = Setup::default();
        let fen_input = Fen {
            setup: setup.clone(),
        }
        .to_string();
        Self {
            setup,
            mode: CastlingMode::Standard,
            orientation: Color::White,
            tool: SetupTool::Pointer,
            selected_square: None,
            drag_from_square: None,
            fen_input,
            fen_error: None,
            validation_error: None,
        }
    }
}

impl PositionSetupState {
    /// Initializes editor with the given setup and castling mode.
    pub fn from_setup(setup: Setup, mode: CastlingMode) -> Self {
        let fen_input = Fen {
            setup: setup.clone(),
        }
        .to_string();
        let orientation = setup.turn;
        let mut state = Self {
            setup,
            mode,
            orientation,
            tool: SetupTool::Pointer,
            selected_square: None,
            drag_from_square: None,
            fen_input,
            fen_error: None,
            validation_error: None,
        };
        state.revalidate();
        state
    }

    /// Initializes editor from an active playable chess position.
    pub fn from_chess(pos: &Chess) -> Self {
        let setup = pos.to_setup(EnPassantMode::Always);
        Self::from_setup(setup, pos.castling_mode())
    }

    /// Revalidates the current setup and updates `validation_error`.
    pub fn revalidate(&mut self) {
        match validate_setup(&self.setup, self.mode) {
            Ok(_) => self.validation_error = None,
            Err(err) => self.validation_error = Some(err),
        }
    }

    /// Synchronizes the FEN text field with the current setup.
    pub fn sync_fen(&mut self) {
        self.fen_input = Fen {
            setup: self.setup.clone(),
        }
        .to_string();
        self.fen_error = None;
        self.revalidate();
    }

    /// Applies the typed FEN text to the board setup.
    pub fn apply_fen(&mut self) -> Result<(), String> {
        let trimmed = self.fen_input.trim();
        if trimmed.is_empty() {
            let err = "FEN string cannot be empty.".to_string();
            self.fen_error = Some(err.clone());
            return Err(err);
        }
        match Fen::from_ascii(trimmed.as_bytes()) {
            Ok(parsed) => {
                self.setup = parsed.into_setup();
                self.fen_error = None;
                self.selected_square = None;
                self.drag_from_square = None;
                self.revalidate();
                Ok(())
            }
            Err(err) => {
                let msg = format!("Invalid FEN: {err}");
                self.fen_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Resets the board to orthodox starting position.
    pub fn reset_startpos(&mut self) {
        self.setup = Setup::default();
        self.mode = CastlingMode::Standard;
        self.selected_square = None;
        self.drag_from_square = None;
        self.sync_fen();
    }

    /// Clears all pieces from the board and wipes castling and en passant.
    pub fn clear_board(&mut self) {
        self.setup.board = Board::empty();
        self.setup.castling_rights = Bitboard::EMPTY;
        self.setup.ep_square = None;
        self.setup.halfmoves = 0;
        self.setup.fullmoves = NonZeroU32::MIN;
        self.selected_square = None;
        self.drag_from_square = None;
        self.sync_fen();
    }

    /// Inverts colors of all pieces, turn, and castling rights.
    pub fn invert_colors(&mut self) {
        let mut new_board = Board::empty();
        for sq in Square::ALL {
            if let Some(piece) = self.setup.board.piece_at(sq) {
                let flipped_sq = Square::from_coords(sq.file(), sq.rank().flip_vertical());
                new_board.set_piece_at(flipped_sq, Piece::new(piece.color.other(), piece.role));
            }
        }
        self.setup.board = new_board;
        self.setup.turn = self.setup.turn.other();

        let mut new_castling = Bitboard::EMPTY;
        for sq in Square::ALL {
            if self.setup.castling_rights.contains(sq) {
                let flipped_sq = Square::from_coords(sq.file(), sq.rank().flip_vertical());
                new_castling |= Bitboard::from_square(flipped_sq);
            }
        }
        self.setup.castling_rights = new_castling;
        self.setup.ep_square = self
            .setup
            .ep_square
            .map(|sq| Square::from_coords(sq.file(), sq.rank().flip_vertical()));
        self.selected_square = None;
        self.drag_from_square = None;
        self.sync_fen();
    }

    /// Checks if a castling right is currently active.
    pub fn has_castling(&self, color: Color, side: CastlingSide) -> bool {
        let castles = Castles::from_setup(&self.setup, self.mode);
        castles.rook(color, side).is_some_and(|rook| {
            self.setup.castling_rights.contains(rook)
                && self.setup.board.piece_at(rook) == Some(Piece::new(color, Role::Rook))
        })
    }

    /// Toggles castling right for the given color and side.
    pub fn toggle_castling(&mut self, color: Color, side: CastlingSide) {
        let back = color.fold_wb(Rank::First, Rank::Eighth);
        match self.mode {
            CastlingMode::Standard => {
                let file = match side {
                    CastlingSide::KingSide => File::H,
                    CastlingSide::QueenSide => File::A,
                };
                let rook_sq = Square::from_coords(file, back);
                if self.setup.castling_rights.contains(rook_sq) {
                    self.setup.castling_rights &= !Bitboard::from_square(rook_sq);
                } else {
                    self.setup.castling_rights |= Bitboard::from_square(rook_sq);
                }
            }
            CastlingMode::Chess960 => {
                if let Some(king_sq) = self.setup.board.king_of(color) {
                    let mut candidate = None;
                    match side {
                        CastlingSide::KingSide => {
                            for f in (king_sq.file().to_u32() + 1)..8 {
                                let sq = Square::from_coords(File::new(f), back);
                                if self.setup.board.piece_at(sq)
                                    == Some(Piece::new(color, Role::Rook))
                                {
                                    candidate = Some(sq);
                                }
                            }
                        }
                        CastlingSide::QueenSide => {
                            for f in (0..king_sq.file().to_u32()).rev() {
                                let sq = Square::from_coords(File::new(f), back);
                                if self.setup.board.piece_at(sq)
                                    == Some(Piece::new(color, Role::Rook))
                                {
                                    candidate = Some(sq);
                                }
                            }
                        }
                    }
                    if let Some(rook_sq) = candidate {
                        if self.setup.castling_rights.contains(rook_sq) {
                            self.setup.castling_rights &= !Bitboard::from_square(rook_sq);
                        } else {
                            self.setup.castling_rights |= Bitboard::from_square(rook_sq);
                        }
                    }
                }
            }
        }
        self.sync_fen();
    }

    /// Sets or clears the en passant file according to side to move.
    pub fn set_ep_file(&mut self, file: Option<File>) {
        self.setup.ep_square = file.map(|f| {
            let rank = match self.setup.turn {
                Color::White => Rank::Sixth,
                Color::Black => Rank::Third,
            };
            Square::from_coords(f, rank)
        });
        self.sync_fen();
    }

    /// Current en passant file, if an en passant square is set.
    pub fn ep_file(&self) -> Option<File> {
        self.setup.ep_square.map(|sq| sq.file())
    }

    /// Converts this setup into a validated new game with PGN headers.
    pub fn to_game(&self, date_str: &str) -> Result<Game, String> {
        let pos = validate_setup(&self.setup, self.mode)?;
        let fen_str = Fen {
            setup: self.setup.clone(),
        }
        .to_string();
        let mut headers = vec![
            ("Event".to_string(), "Setup position".to_string()),
            ("Site".to_string(), "?".to_string()),
            ("Date".to_string(), date_str.to_string()),
            ("Round".to_string(), "?".to_string()),
            ("White".to_string(), "?".to_string()),
            ("Black".to_string(), "?".to_string()),
            ("Result".to_string(), "*".to_string()),
            ("SetUp".to_string(), "1".to_string()),
            ("FEN".to_string(), fen_str),
        ];
        if self.mode == CastlingMode::Chess960 {
            headers.push(("Variant".to_string(), "Chess960".to_string()));
        }
        Ok(Game {
            headers,
            nodes: vec![Node {
                mv: None,
                comment: String::new(),
                clock: None,
                eval: None,
                shapes: Vec::new(),
                nags: Vec::new(),
                parent: None,
                children: Vec::new(),
                pos,
            }],
            errors: Vec::new(),
        })
    }
}

/// Validates whether a [`Setup`] produces a legal playable [`Chess`] position.
pub fn validate_setup(setup: &Setup, mode: CastlingMode) -> Result<Chess, String> {
    let white_kings = Square::ALL
        .into_iter()
        .filter(|&sq| setup.board.piece_at(sq) == Some(Piece::new(Color::White, Role::King)))
        .count();
    let black_kings = Square::ALL
        .into_iter()
        .filter(|&sq| setup.board.piece_at(sq) == Some(Piece::new(Color::Black, Role::King)))
        .count();

    if white_kings == 0 {
        return Err("A White king is required.".to_string());
    }
    if white_kings > 1 {
        return Err("Only one White king is allowed.".to_string());
    }
    if black_kings == 0 {
        return Err("A Black king is required.".to_string());
    }
    if black_kings > 1 {
        return Err("Only one Black king is allowed.".to_string());
    }

    for file in File::ALL {
        for rank in [Rank::First, Rank::Eighth] {
            let sq = Square::from_coords(file, rank);
            if let Some(piece) = setup.board.piece_at(sq)
                && piece.role == Role::Pawn
            {
                return Err(format!(
                    "Pawns cannot be placed on the first or eighth rank ({sq})."
                ));
            }
        }
    }

    Chess::from_setup(setup.clone(), mode).map_err(|err| {
        let msg = err.to_string();
        if msg.contains("The board is invalid") || msg.contains("check") {
            "The position is illegal (for example, the opponent king is in check or the kings are adjacent)."
                .to_string()
        } else {
            msg
        }
    })
}

/// User action outcome from the position setup dialog.
pub enum SetupAction {
    None,
    StartGame(Game),
    Cancel,
}

/// Draws the board setup dialog.
pub fn draw_setup_dialog(
    ctx: &egui::Context,
    open: &mut bool,
    state: &mut PositionSetupState,
    textures: &PieceTextures,
    date_str: &str,
) -> SetupAction {
    if !*open {
        return SetupAction::None;
    }

    let mut action = SetupAction::None;
    let mut dialog_open = *open;

    egui::Window::new("Set up position")
        .open(&mut dialog_open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(560.0);

            ui.horizontal(|ui| {
                // Left pane: Board
                ui.vertical(|ui| {
                    let side = 320.0;
                    draw_setup_board(ui, state, textures, side);

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Starting position").clicked() {
                            state.reset_startpos();
                        }
                        if ui.button("Clear board").clicked() {
                            state.clear_board();
                        }
                        if ui.button("Invert colors").clicked() {
                            state.invert_colors();
                        }
                        if ui.button("Flip board").clicked() {
                            state.orientation = state.orientation.other();
                        }
                    });
                });

                ui.add_space(12.0);

                // Right pane: Tools and controls
                ui.vertical(|ui| {
                    ui.heading("Piece palette");
                    draw_palette(ui, state, textures);

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // Side to move
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Side to move:").strong());
                        if ui
                            .selectable_label(state.setup.turn == Color::White, "White")
                            .clicked()
                            && state.setup.turn != Color::White
                        {
                            state.setup.turn = Color::White;
                            state.sync_fen();
                        }
                        if ui
                            .selectable_label(state.setup.turn == Color::Black, "Black")
                            .clicked()
                            && state.setup.turn != Color::Black
                        {
                            state.setup.turn = Color::Black;
                            state.sync_fen();
                        }
                    });

                    ui.add_space(6.0);

                    // Castling rights
                    ui.label(RichText::new("Castling rights:").strong());
                    ui.horizontal(|ui| {
                        let mut w_short = state.has_castling(Color::White, CastlingSide::KingSide);
                        if ui.checkbox(&mut w_short, "White O-O").changed() {
                            state.toggle_castling(Color::White, CastlingSide::KingSide);
                        }
                        let mut w_long = state.has_castling(Color::White, CastlingSide::QueenSide);
                        if ui.checkbox(&mut w_long, "White O-O-O").changed() {
                            state.toggle_castling(Color::White, CastlingSide::QueenSide);
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut b_short = state.has_castling(Color::Black, CastlingSide::KingSide);
                        if ui.checkbox(&mut b_short, "Black O-O").changed() {
                            state.toggle_castling(Color::Black, CastlingSide::KingSide);
                        }
                        let mut b_long = state.has_castling(Color::Black, CastlingSide::QueenSide);
                        if ui.checkbox(&mut b_long, "Black O-O-O").changed() {
                            state.toggle_castling(Color::Black, CastlingSide::QueenSide);
                        }
                    });

                    ui.add_space(6.0);

                    // En passant
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("En passant:").strong());
                        let current_ep = state.ep_file();
                        if ui
                            .selectable_label(current_ep.is_none(), "-")
                            .on_hover_text("No en passant target")
                            .clicked()
                        {
                            state.set_ep_file(None);
                        }
                        for file_idx in 0..8 {
                            let f = File::new(file_idx);
                            let label = format!("{}", char::from(b'a' + file_idx as u8));
                            if ui
                                .selectable_label(current_ep == Some(f), &label)
                                .on_hover_text(format!("En passant capture on {label}-file"))
                                .clicked()
                            {
                                state.set_ep_file(Some(f));
                            }
                        }
                    });

                    ui.add_space(6.0);

                    // Variant
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Variant:").strong());
                        if ui
                            .selectable_label(state.mode == CastlingMode::Standard, "Standard")
                            .clicked()
                        {
                            state.mode = CastlingMode::Standard;
                            state.sync_fen();
                        }
                        if ui
                            .selectable_label(state.mode == CastlingMode::Chess960, "Chess960")
                            .clicked()
                        {
                            state.mode = CastlingMode::Chess960;
                            state.sync_fen();
                        }
                    });

                    ui.add_space(6.0);

                    // Move numbers
                    ui.horizontal(|ui| {
                        ui.label("Halfmove clock:");
                        let mut half = state.setup.halfmoves;
                        if ui
                            .add(egui::DragValue::new(&mut half).range(0..=100))
                            .changed()
                        {
                            state.setup.halfmoves = half;
                            state.sync_fen();
                        }
                        ui.add_space(8.0);
                        ui.label("Move #:");
                        let mut full = state.setup.fullmoves.get();
                        if ui
                            .add(egui::DragValue::new(&mut full).range(1..=9999))
                            .changed()
                        {
                            if let Some(nz) = NonZeroU32::new(full) {
                                state.setup.fullmoves = nz;
                            }
                            state.sync_fen();
                        }
                    });
                });
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // FEN row
            ui.horizontal(|ui| {
                ui.label(RichText::new("FEN:").strong());
                let response = ui.add(
                    TextEdit::singleline(&mut state.fen_input)
                        .desired_width(ui.available_width() - 140.0),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let _ = state.apply_fen();
                }
                if ui.button("Load FEN").clicked() {
                    let _ = state.apply_fen();
                }
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(state.fen_input.clone());
                }
            });

            if let Some(err) = &state.fen_error {
                ui.label(RichText::new(err).color(RED).small());
            }

            ui.add_space(6.0);

            // Live validation feedback
            ui.horizontal(|ui| {
                if let Some(err) = &state.validation_error {
                    ui.label(RichText::new(format!("⚠ {err}")).color(GOLD).strong());
                } else {
                    ui.label(RichText::new("✓ Position is legal").color(GREEN).strong());
                }
            });

            ui.add_space(8.0);

            // Dialog buttons
            ui.horizontal(|ui| {
                let can_start = state.validation_error.is_none();
                if ui
                    .add_enabled(can_start, egui::Button::new("Start game from position"))
                    .clicked()
                {
                    match state.to_game(date_str) {
                        Ok(game) => action = SetupAction::StartGame(game),
                        Err(err) => state.validation_error = Some(err),
                    }
                }
                if ui.button("Cancel").clicked() {
                    action = SetupAction::Cancel;
                }
            });
        });

    if !dialog_open {
        action = SetupAction::Cancel;
    }

    *open = match action {
        SetupAction::StartGame(_) | SetupAction::Cancel => false,
        SetupAction::None => dialog_open,
    };

    action
}

fn draw_setup_board(
    ui: &mut Ui,
    state: &mut PositionSetupState,
    textures: &PieceTextures,
    side: f32,
) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let cell = side / 8.0;

    let pointer = response.interact_pointer_pos();
    let hovered = pointer.and_then(|p| square_at_pos(rect, cell, p, state.orientation));

    // Handle right-click removal
    if response.secondary_clicked()
        && let Some(sq) = hovered
    {
        state.setup.board.remove_piece_at(sq);
        state.setup.castling_rights &= !Bitboard::from_square(sq);
        state.selected_square = None;
        state.sync_fen();
    }

    // Handle left click and drag
    if response.drag_started()
        && let Some(sq) = hovered
        && state.tool == SetupTool::Pointer
        && state.setup.board.piece_at(sq).is_some()
    {
        state.drag_from_square = Some(sq);
        state.selected_square = Some(sq);
    }

    if response.drag_stopped() {
        if let Some(from) = state.drag_from_square.take() {
            if let Some(to) = hovered {
                if from != to
                    && let Some(piece) = state.setup.board.piece_at(from)
                {
                    state.setup.board.remove_piece_at(from);
                    state.setup.board.set_piece_at(to, piece);
                    if state.setup.castling_rights.contains(from) {
                        state.setup.castling_rights &= !Bitboard::from_square(from);
                        state.setup.castling_rights |= Bitboard::from_square(to);
                    }
                    state.sync_fen();
                }
            } else {
                // Dragged off board -> delete
                state.setup.board.remove_piece_at(from);
                state.setup.castling_rights &= !Bitboard::from_square(from);
                state.sync_fen();
            }
        }
    } else if response.clicked()
        && let Some(sq) = hovered
    {
        match state.tool {
            SetupTool::Piece(piece) => {
                state.setup.board.set_piece_at(sq, piece);
                state.selected_square = None;
                state.sync_fen();
            }
            SetupTool::Eraser => {
                state.setup.board.remove_piece_at(sq);
                state.setup.castling_rights &= !Bitboard::from_square(sq);
                state.selected_square = None;
                state.sync_fen();
            }
            SetupTool::Pointer => match state.selected_square {
                Some(selected) if selected != sq => {
                    if let Some(piece) = state.setup.board.piece_at(selected) {
                        state.setup.board.remove_piece_at(selected);
                        state.setup.board.set_piece_at(sq, piece);
                        if state.setup.castling_rights.contains(selected) {
                            state.setup.castling_rights &= !Bitboard::from_square(selected);
                            state.setup.castling_rights |= Bitboard::from_square(sq);
                        }
                        state.selected_square = None;
                        state.sync_fen();
                    }
                }
                Some(selected) if selected == sq => {
                    state.selected_square = None;
                }
                _ => {
                    if state.setup.board.piece_at(sq).is_some() {
                        state.selected_square = Some(sq);
                    } else {
                        state.selected_square = None;
                    }
                }
            },
        }
    }

    // Paint squares and pieces
    for row in 0..8 {
        for col in 0..8 {
            let sq = square_at(col, row, state.orientation);
            let cell_rect = Rect::from_min_size(
                pos2(
                    rect.left() + col as f32 * cell,
                    rect.top() + row as f32 * cell,
                ),
                Vec2::splat(cell),
            );
            let dark = is_dark(sq);
            painter.rect_filled(cell_rect, 0.0, if dark { DARK } else { LIGHT });

            if state.selected_square == Some(sq) {
                painter.rect_filled(cell_rect, 0.0, SELECTED);
            }

            if let Some(piece) = state.setup.board.piece_at(sq) {
                // If dragging from this square, paint slightly transparent
                let is_dragging = state.drag_from_square == Some(sq);
                if let Some(tex) = textures.get(piece) {
                    let tint = if is_dragging {
                        Color32::from_white_alpha(128)
                    } else {
                        Color32::WHITE
                    };
                    painter.image(
                        tex.id(),
                        cell_rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        tint,
                    );
                }
            }
        }
    }

    // Paint dragged piece at pointer position
    if let Some(from) = state.drag_from_square
        && let Some(p) = pointer
        && let Some(piece) = state.setup.board.piece_at(from)
        && let Some(tex) = textures.get(piece)
    {
        let drag_rect = Rect::from_center_size(p, Vec2::splat(cell * 1.1));
        painter.image(
            tex.id(),
            drag_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // Paint border
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, Color32::from_gray(100)),
        StrokeKind::Outside,
    );
}

fn draw_palette(ui: &mut Ui, state: &mut PositionSetupState, textures: &PieceTextures) {
    let piece_size = 32.0;

    ui.horizontal(|ui| {
        ui.label(RichText::new("White:").small());
        for role in [
            Role::King,
            Role::Queen,
            Role::Rook,
            Role::Bishop,
            Role::Knight,
            Role::Pawn,
        ] {
            let piece = Piece::new(Color::White, role);
            let selected = state.tool == SetupTool::Piece(piece);
            if let Some(tex) = textures.get(piece) {
                let img = egui::Image::from_texture(tex).fit_to_exact_size(Vec2::splat(piece_size));
                let resp = ui.add(egui::Button::image(img).selected(selected));
                if resp.clicked() {
                    state.tool = if selected {
                        SetupTool::Pointer
                    } else {
                        SetupTool::Piece(piece)
                    };
                }
            }
        }
    });

    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label(RichText::new("Black:").small());
        for role in [
            Role::King,
            Role::Queen,
            Role::Rook,
            Role::Bishop,
            Role::Knight,
            Role::Pawn,
        ] {
            let piece = Piece::new(Color::Black, role);
            let selected = state.tool == SetupTool::Piece(piece);
            if let Some(tex) = textures.get(piece) {
                let img = egui::Image::from_texture(tex).fit_to_exact_size(Vec2::splat(piece_size));
                let resp = ui.add(egui::Button::image(img).selected(selected));
                if resp.clicked() {
                    state.tool = if selected {
                        SetupTool::Pointer
                    } else {
                        SetupTool::Piece(piece)
                    };
                }
            }
        }
    });

    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui
            .selectable_label(state.tool == SetupTool::Pointer, "Pointer (Move)")
            .clicked()
        {
            state.tool = SetupTool::Pointer;
        }
        if ui
            .selectable_label(state.tool == SetupTool::Eraser, "Eraser (Clear)")
            .clicked()
        {
            state.tool = SetupTool::Eraser;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::Position;

    #[test]
    fn default_setup_is_valid_orthodox_startpos() {
        let state = PositionSetupState::default();
        assert_eq!(state.mode, CastlingMode::Standard);
        assert_eq!(state.setup.turn, Color::White);
        assert!(validate_setup(&state.setup, state.mode).is_ok());
        assert_eq!(
            state.fen_input,
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
        );
    }

    #[test]
    fn clear_board_and_piece_placement() {
        let mut state = PositionSetupState::default();
        state.clear_board();
        assert!(state.validation_error.is_some());
        assert_eq!(state.fen_input, "8/8/8/8/8/8/8/8 w - - 0 1");

        // Place Kings
        state
            .setup
            .board
            .set_piece_at(Square::E1, Piece::new(Color::White, Role::King));
        state
            .setup
            .board
            .set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
        state.sync_fen();
        assert_eq!(state.fen_input, "4k3/8/8/8/8/8/8/4K3 w - - 0 1");
        assert!(validate_setup(&state.setup, state.mode).is_ok());

        // Add 3 Queens and verify multiple pieces of same type are valid
        state
            .setup
            .board
            .set_piece_at(Square::D1, Piece::new(Color::White, Role::Queen));
        state
            .setup
            .board
            .set_piece_at(Square::C1, Piece::new(Color::White, Role::Queen));
        state
            .setup
            .board
            .set_piece_at(Square::B1, Piece::new(Color::White, Role::Queen));
        state.sync_fen();
        assert!(validate_setup(&state.setup, state.mode).is_ok());
    }

    #[test]
    fn illegal_setups_are_rejected_with_clear_messages() {
        let mut state = PositionSetupState::default();
        state.clear_board();

        // 1. Missing white king
        state
            .setup
            .board
            .set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert_eq!(err, "A White king is required.");

        // 2. Missing black king
        state.clear_board();
        state
            .setup
            .board
            .set_piece_at(Square::E1, Piece::new(Color::White, Role::King));
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert_eq!(err, "A Black king is required.");

        // 3. Multiple white kings
        state
            .setup
            .board
            .set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
        state
            .setup
            .board
            .set_piece_at(Square::D1, Piece::new(Color::White, Role::King));
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert_eq!(err, "Only one White king is allowed.");

        // 4. Pawn on first rank
        state.clear_board();
        state
            .setup
            .board
            .set_piece_at(Square::E1, Piece::new(Color::White, Role::King));
        state
            .setup
            .board
            .set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
        state
            .setup
            .board
            .set_piece_at(Square::A1, Piece::new(Color::White, Role::Pawn));
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert!(err.contains("first or eighth rank"));

        // 5. Pawn on eighth rank
        state.setup.board.remove_piece_at(Square::A1);
        state
            .setup
            .board
            .set_piece_at(Square::H8, Piece::new(Color::Black, Role::Pawn));
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert!(err.contains("first or eighth rank"));

        // 6. Opponent king in check
        state.setup.board.remove_piece_at(Square::A1);
        state.setup.board.remove_piece_at(Square::H8);
        // White to move, but Black is in check from a rook on e2
        state
            .setup
            .board
            .set_piece_at(Square::E2, Piece::new(Color::White, Role::Rook));
        state.setup.turn = Color::White;
        let err = validate_setup(&state.setup, state.mode).unwrap_err();
        assert!(
            err.to_lowercase().contains("illegal") || err.to_lowercase().contains("invalid"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn fen_round_trip_and_editing() {
        let mut state = PositionSetupState {
            fen_input: "r3k2r/pPqp1ppp/8/4pP2/8/8/P1P2PPP/R3K2R w KQkq e6 0 1".to_string(),
            ..Default::default()
        };
        state.apply_fen().unwrap();
        assert_eq!(state.setup.ep_square, Some(Square::E6));
        assert_eq!(state.setup.turn, Color::White);
        assert!(state.has_castling(Color::White, CastlingSide::KingSide));
        assert!(state.has_castling(Color::White, CastlingSide::QueenSide));
        assert!(state.has_castling(Color::Black, CastlingSide::KingSide));
        assert!(state.has_castling(Color::Black, CastlingSide::QueenSide));
        assert_eq!(
            Fen {
                setup: state.setup.clone()
            }
            .to_string(),
            "r3k2r/pPqp1ppp/8/4pP2/8/8/P1P2PPP/R3K2R w KQkq e6 0 1"
        );
    }

    #[test]
    fn chess960_mode_validation() {
        let mut state = PositionSetupState {
            mode: CastlingMode::Chess960,
            fen_input: "qnbnrkrb/pppppppp/8/8/8/8/PPPPPPPP/QNBNRKRB w GEge - 0 1".to_string(),
            ..Default::default()
        };
        state.apply_fen().unwrap();
        assert!(validate_setup(&state.setup, state.mode).is_ok());

        let game = state.to_game("2026.11.02").unwrap();
        assert_eq!(game.header("Variant"), Some("Chess960"));
        assert_eq!(game.header("SetUp"), Some("1"));
        assert!(game.header("FEN").is_some());
    }

    #[test]
    fn starting_a_game_creates_valid_game_with_setup_tags() {
        let mut state = PositionSetupState {
            fen_input: "8/8/8/8/8/8/4K3/7k w - - 0 1".to_string(),
            ..Default::default()
        };
        state.apply_fen().unwrap();
        assert!(validate_setup(&state.setup, state.mode).is_ok());

        let game = state.to_game("2026.11.02").unwrap();
        assert_eq!(game.header("SetUp"), Some("1"));
        assert_eq!(game.header("FEN"), Some("8/8/8/8/8/8/4K3/7k w - - 0 1"));
        assert_eq!(game.nodes.len(), 1);
        assert_eq!(game.nodes[0].pos.turn(), Color::White);
    }
}
