use std::collections::HashMap;

use crate::chess::{Chess, Color, File, Position, Rank, Square};
use crate::domain::{Shape, ShapeColor};
use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};

use crate::ui::pieces::PieceTextures;

pub(crate) const LIGHT: Color32 = Color32::from_rgb(0xDF, 0xCF, 0xB8);
pub(crate) const DARK: Color32 = Color32::from_rgb(0x9E, 0x7B, 0x56);
const LAST_MOVE: Color32 = Color32::from_rgba_premultiplied(0x58, 0x54, 0x15, 0x70);
pub(crate) const SELECTED: Color32 = Color32::from_rgba_premultiplied(0x12, 0x51, 0x24, 0x90);
const TARGET: Color32 = Color32::from_rgba_premultiplied(0x06, 0x13, 0x06, 0x66);
const DROP_TARGET: Color32 = Color32::from_rgba_premultiplied(0xE6, 0xF0, 0xDA, 0x38);

pub struct BoardAnimData {
    time_elapsed: f32,
    animated: HashMap<Square, Square>,
}

impl BoardAnimData {
    pub fn from_animated_pairs(pairs: HashMap<Square, Square>) -> Self {
        Self {
            time_elapsed: 0.0,
            animated: pairs,
        }
    }

    pub fn progress(&self) -> f32 {
        let normalized = self.time_elapsed / Self::duration();
        if normalized <= 0.5 {
            normalized * normalized * 2.0
        } else {
            let normalized_mirrored = 1.0 - normalized;
            1.0 - (2.0 * normalized_mirrored * normalized_mirrored)
        }
    }

    pub fn duration() -> f32 {
        return 0.2;
    }

    pub fn handle_dt(&mut self, dt: f32) -> bool {
        self.time_elapsed += dt;
        return self.time_elapsed > Self::duration();
    }
}

/// One user interaction on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardEvent {
    Click(Square),
    DragStart(Square),
    Drop {
        from: Square,
        to: Square,
    },
    DragCancel,
    /// Mark one square with a Lichess/ChessBase colour.
    ShapeSquare {
        square: Square,
        color: ShapeColor,
    },
    /// A drawing drag began on `square`; the app keeps it until the release.
    ShapeDragStart(Square),
    /// Draw an arrow between two squares.
    ShapeArrow {
        from: Square,
        to: Square,
        color: ShapeColor,
    },
    /// Horizontal touch swipe right (previous move).
    SwipePrev,
    /// Horizontal touch swipe left (next move).
    SwipeNext,
}

pub struct BoardState<'a> {
    pub pos: &'a Chess,
    pub orientation: Color,
    pub last_move: Option<(Square, Square)>,
    pub selected: Option<Square>,
    pub targets: &'a [Square],
    /// Coloured squares and arrows stored on the current position.
    pub shapes: &'a [Shape],
    /// Square where a secondary-button or Shift drawing drag started.
    pub shape_drag_from: Option<Square>,
    /// Square currently being dragged, if any. Kept by the app across frames.
    pub drag_from: Option<Square>,
    /// Active touch/stylus shape-drawing colour (primary drags/taps draw shapes instead of moving pieces).
    pub draw_mode: Option<ShapeColor>,
    /// Allow horizontal swipes on non-movable squares to step backward/forward.
    pub touch_swipe: bool,
    /// progress, from, to
    pub animation: &'a Option<BoardAnimData>,
}

pub fn mix_rects_for_anim(anim_prog: f32, rect1: Rect, rect2: Rect) -> Rect {
    if anim_prog < 0.0 {
        eprintln!("Invalid animation progress: {anim_prog}.");
        return rect1;
    } else if anim_prog > 1.0 {
        eprintln!("Invalid animation progress: {anim_prog}.");
        return rect2;
    }
    let center1 = rect1.center();
    let center2 = rect2.center();
    let anim_left = 1.0 - anim_prog;
    let center_mixed = Pos2 {
        x: center1.x * anim_left + center2.x * anim_prog,
        y: center1.y * anim_left + center2.y * anim_prog,
    };
    return Rect::from_center_size(center_mixed, rect2.size());
}

/// Draw an interactive board. Pieces can be moved by clicking or by dragging.
pub fn draw(ui: &mut Ui, tex: &PieceTextures, st: BoardState<'_>, side: f32) -> Option<BoardEvent> {
    let side = side.clamp(64.0, 4096.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let cell = side / 8.0;

    let square_rect = |sq: Square| -> Rect {
        let (col, row) = coords(sq, st.orientation);
        Rect::from_min_size(
            pos2(
                rect.left() + col as f32 * cell,
                rect.top() + row as f32 * cell,
            ),
            Vec2::splat(cell),
        )
    };

    let pointer = response.interact_pointer_pos();
    let hovered = pointer.and_then(|p| square_at_pos(rect, cell, p, st.orientation));
    let press_origin = ui.input(|i| i.pointer.press_origin());
    let drag_origin = press_origin
        .and_then(|p| square_at_pos(rect, cell, p, st.orientation))
        .or(hovered);
    let (key_g, key_r, key_b, key_y) = ui.input(|i| {
        (
            i.key_down(egui::Key::G),
            i.key_down(egui::Key::R),
            i.key_down(egui::Key::B),
            i.key_down(egui::Key::Y),
        )
    });
    let modifiers = ui.input(|i| i.modifiers);
    let has_touch = ui.input(|i| i.any_touches());
    let drawing_color = if let Some(mode_color) = st.draw_mode {
        mode_color
    } else if key_g {
        ShapeColor::Green
    } else if key_r {
        ShapeColor::Red
    } else if key_b {
        ShapeColor::Blue
    } else if key_y {
        ShapeColor::Yellow
    } else {
        shape_color(&modifiers)
    };
    let shape_with_primary = st.draw_mode.is_some()
        || modifiers.shift
        || modifiers.ctrl
        || modifiers.alt
        || key_g
        || key_r
        || key_b
        || key_y;
    let secondary_drag = response.drag_started_by(PointerButton::Secondary)
        || response.dragged_by(PointerButton::Secondary)
        || response.drag_stopped_by(PointerButton::Secondary);
    let shape_dragging = secondary_drag
        || (shape_with_primary
            && (response.dragged() || response.drag_started() || response.drag_stopped()));
    let just_started = (!shape_dragging)
        .then(|| response.drag_started().then_some(drag_origin).flatten())
        .flatten()
        .filter(|sq| is_movable(st.pos, *sq));
    let active_drag = (!shape_with_primary
        && (response.dragged() || response.drag_started() || response.drag_stopped()))
    .then(|| st.drag_from.or(just_started))
    .flatten();

    let swipe_origin_id = response.id.with("touch_swipe_origin");
    if st.touch_swipe
        && has_touch
        && !shape_dragging
        && active_drag.is_none()
        && response.drag_started()
    {
        if let Some(p) = press_origin.or(pointer) {
            ui.data_mut(|d| d.insert_temp(swipe_origin_id, p));
        }
    } else if !response.dragged() && !response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<Pos2>(swipe_origin_id));
    }

    for row in 0..8 {
        for col in 0..8 {
            let sq = square_at(col, row, st.orientation);
            let cell_rect = Rect::from_min_size(
                pos2(
                    rect.left() + col as f32 * cell,
                    rect.top() + row as f32 * cell,
                ),
                Vec2::splat(cell),
            );
            let dark = is_dark(sq);
            painter.rect_filled(cell_rect, 0.0, if dark { DARK } else { LIGHT });

            if let Some((from, to)) = st.last_move
                && (sq == from || sq == to)
            {
                painter.rect_filled(cell_rect, 0.0, LAST_MOVE);
            }
            if st.selected == Some(sq) {
                painter.rect_filled(cell_rect, 0.0, SELECTED);
            }
        }
    }

    // Coordinates, drawn inside the edge squares.
    let font = egui::FontId::proportional((cell * 0.16).max(9.0));
    for i in 0..8u32 {
        let file_sq = Square::from_coords(File::new(i), Rank::First);
        let (fc, fr) = coords(file_sq, st.orientation);
        painter.text(
            pos2(
                rect.left() + fc as f32 * cell + cell * 0.93,
                rect.top() + fr as f32 * cell + cell * 0.93,
            ),
            egui::Align2::RIGHT_BOTTOM,
            File::new(i).to_string(),
            font.clone(),
            contrast_text(is_dark(file_sq)),
        );

        let rank_sq = Square::from_coords(File::A, Rank::new(i));
        let (rc, rr) = coords(rank_sq, st.orientation);
        painter.text(
            pos2(
                rect.left() + rc as f32 * cell + cell * 0.07,
                rect.top() + rr as f32 * cell + cell * 0.07,
            ),
            egui::Align2::LEFT_TOP,
            (i + 1).to_string(),
            font.clone(),
            contrast_text(is_dark(rank_sq)),
        );
    }

    for sq in st.targets {
        let r = square_rect(*sq);
        if st.pos.board().piece_at(*sq).is_some() {
            painter.circle_stroke(r.center(), cell * 0.42, Stroke::new(cell * 0.07, TARGET));
        } else {
            painter.circle_filled(r.center(), cell * 0.14, TARGET);
        }
    }

    if active_drag.is_some()
        && let Some(to) = hovered
        && st.targets.contains(&to)
    {
        let r = square_rect(to);
        painter.rect_filled(r, 0.0, DROP_TARGET);
        painter.rect_stroke(
            r.shrink(2.0),
            0.0,
            Stroke::new(2.0, Color32::from_white_alpha(180)),
            StrokeKind::Inside,
        );
    }

    for &shape in st.shapes {
        if matches!(shape, Shape::Square { .. }) {
            draw_shape(&painter, shape, st.orientation, rect, cell);
        }
    }

    for row in 0..8 {
        for col in 0..8 {
            let sq = square_at(col, row, st.orientation);
            if active_drag == Some(sq) {
                continue;
            }
            if let Some(piece) = st.pos.board().piece_at(sq)
                && let Some(texture) = tex.get(piece)
            {
                let mut r = square_rect(sq).shrink(cell * 0.04);
                if let Some(anim) = &st.animation {
                    if let Some(oldsq) = anim.animated.get(&sq) {
                        let oldr = square_rect(*oldsq).shrink(cell * 0.04);
                        r = mix_rects_for_anim(anim.progress(), oldr, r);
                    }
                }
                painter.image(
                    texture.id(),
                    r,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
    }

    if let Some(from) = active_drag
        && let Some(piece) = st.pos.board().piece_at(from)
        && let Some(texture) = tex.get(piece)
    {
        let center = pointer.unwrap_or_else(|| square_rect(from).center());
        let r = Rect::from_center_size(center, Vec2::splat(cell * 0.96));
        painter.image(
            texture.id(),
            r,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::from_white_alpha(235),
        );
    }

    for &shape in st.shapes {
        if matches!(shape, Shape::Arrow { .. }) {
            draw_shape(&painter, shape, st.orientation, rect, cell);
        }
    }

    if active_drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered.is_some_and(|sq| is_movable(st.pos, sq)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }

    let shape_drag_id = response.id.with("shape_drag_origin");
    if (response.drag_started_by(PointerButton::Secondary)
        || (shape_with_primary && response.drag_started()))
        && let Some(from) = drag_origin
    {
        let idx = (from.file().to_u32() + from.rank().to_u32() * 8) as u8;
        ui.data_mut(|d| d.insert_temp(shape_drag_id, idx));
    }

    let dragging_shape_from: Option<Square> = ui
        .data(|d| d.get_temp::<u8>(shape_drag_id))
        .map(|idx| Square::new(u32::from(idx)));
    if let Some(from) = dragging_shape_from.or(st.shape_drag_from)
        && let Some(to) = hovered
        && from != to
    {
        draw_shape(
            &painter,
            Shape::Arrow {
                from,
                to,
                color: drawing_color,
            },
            st.orientation,
            rect,
            cell,
        );
    }

    if response.drag_stopped_by(PointerButton::Secondary)
        || (shape_with_primary && response.drag_stopped())
    {
        let stored_from: Option<Square> = ui
            .data_mut(|d| d.remove_temp::<u8>(shape_drag_id))
            .map(|idx| Square::new(u32::from(idx)));
        if let Some(from) = stored_from.or(st.shape_drag_from).or(drag_origin) {
            if let Some(to) = hovered
                && to != from
            {
                return Some(BoardEvent::ShapeArrow {
                    from,
                    to,
                    color: drawing_color,
                });
            }
            return Some(BoardEvent::ShapeSquare {
                square: from,
                color: drawing_color,
            });
        }
    }

    if (response.secondary_clicked() || (shape_with_primary && response.clicked()))
        && let Some(sq) = hovered
    {
        ui.data_mut(|d| d.remove_temp::<u8>(shape_drag_id));
        return Some(BoardEvent::ShapeSquare {
            square: sq,
            color: drawing_color,
        });
    }

    if response.drag_stopped() {
        let swipe_start: Option<Pos2> = ui.data_mut(|d| d.remove_temp::<Pos2>(swipe_origin_id));
        if let Some(from) = active_drag {
            if let Some(to) = hovered
                && to != from
            {
                return Some(BoardEvent::Drop { from, to });
            }
            return Some(BoardEvent::DragCancel);
        }
        if st.touch_swipe
            && has_touch
            && !shape_dragging
            && let (Some(start), Some(end)) = (swipe_start, pointer)
        {
            let delta = end - start;
            if delta.x.abs() > cell * 1.4 && delta.x.abs() > delta.y.abs() * 2.0 {
                return Some(if delta.x > 0.0 {
                    BoardEvent::SwipePrev
                } else {
                    BoardEvent::SwipeNext
                });
            }
        }
    }

    if let Some(from) = just_started {
        return Some(BoardEvent::DragStart(from));
    }

    if response.clicked()
        && let Some(sq) = hovered
    {
        return Some(BoardEvent::Click(sq));
    }

    None
}

/// Lichess drawing modifiers: green by default, red with Ctrl/Shift, blue with
/// Alt/Command, and yellow when one key from each group is held.
fn shape_color(modifiers: &egui::Modifiers) -> ShapeColor {
    let red = modifiers.ctrl || modifiers.shift;
    let blue = modifiers.alt || modifiers.mac_cmd;
    match (red, blue) {
        (true, true) => ShapeColor::Yellow,
        (true, false) => ShapeColor::Red,
        (false, true) => ShapeColor::Blue,
        (false, false) => ShapeColor::Green,
    }
}

fn drawing_color(color: ShapeColor) -> Color32 {
    match color {
        ShapeColor::Green => Color32::from_rgba_unmultiplied(0x15, 0x78, 0x1B, 220),
        ShapeColor::Yellow => Color32::from_rgba_unmultiplied(0xE6, 0x8F, 0x00, 220),
        ShapeColor::Red => Color32::from_rgba_unmultiplied(0x88, 0x20, 0x20, 220),
        ShapeColor::Blue => Color32::from_rgba_unmultiplied(0x00, 0x30, 0x88, 220),
    }
}

fn draw_shape(painter: &egui::Painter, shape: Shape, orientation: Color, rect: Rect, cell: f32) {
    let square_rect = |sq: Square| -> Rect {
        let (col, row) = coords(sq, orientation);
        Rect::from_min_size(
            pos2(
                rect.left() + col as f32 * cell,
                rect.top() + row as f32 * cell,
            ),
            Vec2::splat(cell),
        )
    };
    let color = drawing_color(match shape {
        Shape::Square { color, .. } | Shape::Arrow { color, .. } => color,
    });
    match shape {
        Shape::Square { square, .. } => {
            let r = square_rect(square);
            painter.rect_filled(
                r,
                0.0,
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 110),
            );
            painter.rect_stroke(
                r.shrink(1.5),
                0.0,
                Stroke::new(
                    2.5,
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 220),
                ),
                StrokeKind::Inside,
            );
        }
        Shape::Arrow { from, to, .. } if from != to => {
            let start = square_rect(from).center();
            let end = square_rect(to).center();
            let delta = end - start;
            let length = delta.length();
            if length < 1.0 {
                return;
            }
            let direction = delta / length;
            let shaft_end = end - direction * cell * 0.30;
            let width = cell * 0.12;
            painter.line_segment([start, shaft_end], Stroke::new(width, color));
            let perpendicular = Vec2::new(-direction.y, direction.x);
            let head = vec![
                end,
                shaft_end + perpendicular * cell * 0.20,
                shaft_end - perpendicular * cell * 0.20,
            ];
            painter.add(egui::Shape::convex_polygon(
                head,
                color,
                Stroke::new(0.0, Color32::TRANSPARENT),
            ));
        }
        Shape::Arrow { .. } => {}
    }
}

fn is_movable(pos: &Chess, sq: Square) -> bool {
    pos.board()
        .piece_at(sq)
        .is_some_and(|piece| piece.color == pos.turn())
}

pub(crate) fn square_at_pos(rect: Rect, cell: f32, p: Pos2, orientation: Color) -> Option<Square> {
    let col = ((p.x - rect.left()) / cell).floor();
    let row = ((p.y - rect.top()) / cell).floor();
    if (0.0..8.0).contains(&col) && (0.0..8.0).contains(&row) {
        Some(square_at(col as i32, row as i32, orientation))
    } else {
        None
    }
}

pub(crate) fn is_dark(sq: Square) -> bool {
    (sq.file().to_u32() + sq.rank().to_u32()).is_multiple_of(2)
}

fn contrast_text(dark_square: bool) -> Color32 {
    if dark_square { LIGHT } else { DARK }
}

pub(crate) fn coords(sq: Square, orientation: Color) -> (i32, i32) {
    let file = sq.file().to_u32() as i32;
    let rank = sq.rank().to_u32() as i32;
    match orientation {
        Color::White => (file, 7 - rank),
        Color::Black => (7 - file, rank),
    }
}

pub(crate) fn square_at(col: i32, row: i32, orientation: Color) -> Square {
    let (file, rank) = match orientation {
        Color::White => (col, 7 - row),
        Color::Black => (7 - col, row),
    };
    Square::from_coords(File::new(file as u32), Rank::new(rank as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_orientation_puts_a8_top_left() {
        assert_eq!(square_at(0, 0, Color::White), Square::A8);
        assert_eq!(square_at(7, 0, Color::White), Square::H8);
        assert_eq!(square_at(0, 7, Color::White), Square::A1);
        assert_eq!(square_at(7, 7, Color::White), Square::H1);
        assert_eq!(coords(Square::E4, Color::White), (4, 4));
    }

    #[test]
    fn black_orientation_is_mirrored() {
        assert_eq!(square_at(0, 0, Color::Black), Square::H1);
        assert_eq!(square_at(7, 0, Color::Black), Square::A1);
        assert_eq!(square_at(0, 7, Color::Black), Square::H8);
        assert_eq!(square_at(7, 7, Color::Black), Square::A8);
        assert_eq!(coords(Square::E4, Color::Black), (3, 3));
    }

    #[test]
    fn every_square_survives_a_round_trip() {
        for orientation in [Color::White, Color::Black] {
            for idx in 0..64 {
                let sq = Square::new(idx);
                let (col, row) = coords(sq, orientation);
                assert_eq!(square_at(col, row, orientation), sq);
            }
        }
    }

    #[test]
    fn pointer_positions_map_to_squares() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), Vec2::splat(800.0));
        assert_eq!(
            square_at_pos(rect, 100.0, pos2(15.0, 25.0), Color::White),
            Some(Square::A8)
        );
        assert_eq!(
            square_at_pos(rect, 100.0, pos2(805.0, 815.0), Color::White),
            Some(Square::H1)
        );
        assert_eq!(
            square_at_pos(rect, 100.0, pos2(5.0, 25.0), Color::White),
            None
        );
    }

    #[test]
    fn a1_is_dark_and_h1_is_light() {
        assert!(is_dark(Square::A1));
        assert!(!is_dark(Square::H1));
        assert!(!is_dark(Square::A8));
        assert!(is_dark(Square::H8));
    }

    #[test]
    fn lichess_shape_modifiers_choose_the_colour() {
        use crate::domain::ShapeColor;
        use egui::Modifiers;

        let modifiers = |ctrl: bool, shift: bool, alt: bool| Modifiers {
            ctrl,
            shift,
            alt,
            ..Modifiers::default()
        };
        assert_eq!(
            shape_color(&modifiers(false, false, false)),
            ShapeColor::Green
        );
        assert_eq!(shape_color(&modifiers(true, false, false)), ShapeColor::Red);
        assert_eq!(shape_color(&modifiers(false, true, false)), ShapeColor::Red);
        assert_eq!(
            shape_color(&modifiers(false, false, true)),
            ShapeColor::Blue
        );
        assert_eq!(
            shape_color(&modifiers(true, false, true)),
            ShapeColor::Yellow
        );
        assert_eq!(
            shape_color(&modifiers(false, true, true)),
            ShapeColor::Yellow
        );
    }

    #[test]
    fn draw_mode_draws_square_on_primary_click() {
        let ctx = egui::Context::default();
        let tex = PieceTextures::load(&ctx);
        let pos = Chess::default();
        let center = pos2(120.0, 120.0);
        let draw_once = |events: Vec<egui::Event>| -> Option<BoardEvent> {
            let mut ev = None;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), Vec2::splat(600.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                ev = draw(
                    ui,
                    &tex,
                    BoardState {
                        pos: &pos,
                        orientation: Color::White,
                        last_move: None,
                        selected: None,
                        targets: &[],
                        shapes: &[],
                        shape_drag_from: None,
                        drag_from: None,
                        draw_mode: Some(ShapeColor::Blue),
                        touch_swipe: true,
                        animation: &None,
                    },
                    400.0,
                );
            });
            out.textures_delta.clear();
            ev
        };
        // Frame 1 registers the board rect; frame 2 delivers the click.
        let _ = draw_once(vec![]);
        let _ = draw_once(vec![
            egui::Event::PointerMoved(center),
            egui::Event::PointerButton {
                pos: center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let emitted = draw_once(vec![egui::Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(matches!(
            emitted,
            Some(BoardEvent::ShapeSquare {
                color: ShapeColor::Blue,
                ..
            })
        ));
    }
}
