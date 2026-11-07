//! Vertical evaluation bar rendered beside the chessboard.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};

use crate::eval::BoardEval;

/// Width of the evaluation bar in logical pixels.
pub const WIDTH: f32 = 16.0;
/// Horizontal gap between the evaluation bar and the board.
pub const GAP: f32 = 6.0;

const WHITE_FILL: Color32 = Color32::from_rgb(0xEB, 0xEC, 0xD0);
const BLACK_FILL: Color32 = Color32::from_rgb(0x26, 0x24, 0x21);
const CENTER_TICK: Color32 = Color32::from_rgba_premultiplied(0x77, 0x96, 0x56, 0xB0);

/// Draw a vertical evaluation bar of height `side`.
///
/// White fills from the bottom upward regardless of board orientation.
pub fn draw(ui: &mut Ui, eval: Option<BoardEval>, side: f32) -> egui::Response {
    draw_with_hold(ui, eval, false, side)
}

/// Draw the bar while flagging that `eval` is inherited from an earlier ply
/// (the current position's evaluation is still being computed).
pub fn draw_with_hold(
    ui: &mut Ui,
    eval: Option<BoardEval>,
    is_held: bool,
    side: f32,
) -> egui::Response {
    let side = side.clamp(64.0, 4096.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, side), Sense::click());
    let painter = ui.painter_at(rect);

    let white_fraction = eval
        .map(|e| (e.score.win_percent() / 100.0).clamp(0.0, 1.0) as f32)
        .unwrap_or(0.5);

    let radius = CornerRadius::same(3);
    painter.rect_filled(rect, radius, BLACK_FILL);

    let white_height = (rect.height() * white_fraction).clamp(0.0, rect.height());
    if white_height > 0.0 {
        let split_y = rect.bottom() - white_height;
        let white_rect = Rect::from_min_max(pos2(rect.left(), split_y), rect.right_bottom());
        let white_radius = if white_height >= rect.height() - 0.5 {
            radius
        } else {
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 3,
                se: 3,
            }
        };
        painter.rect_filled(white_rect, white_radius, WHITE_FILL);
    }

    let mid_y = rect.center().y;
    painter.line_segment(
        [
            Pos2::new(rect.left() + 1.0, mid_y),
            Pos2::new(rect.right() - 1.0, mid_y),
        ],
        Stroke::new(1.0, CENTER_TICK),
    );

    let border = ui.visuals().widgets.noninteractive.bg_stroke.color;
    painter.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);

    let tip = match (eval, is_held) {
        (Some(eval), true) => {
            format!("{} · held from previous ply (computing…)", eval.tooltip())
        }
        (Some(eval), false) => eval.tooltip(),
        (None, _) => "Evaluation bar · waiting for evaluation".to_string(),
    };
    response.on_hover_text(tip)
}
