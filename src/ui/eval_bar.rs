//! Vertical evaluation bar rendered beside the chessboard.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};

use crate::eval::BoardEval;

/// Width of the evaluation bar in logical pixels.
pub const WIDTH: f32 = 14.0;
/// Horizontal gap between the evaluation bar and the board.
pub const GAP: f32 = 6.0;

const WHITE_FILL: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF2);
const BLACK_FILL: Color32 = Color32::from_rgb(0x20, 0x24, 0x2C);
const NO_EVAL_FILL: Color32 = Color32::from_rgb(0x3A, 0x40, 0x4C);
const NO_EVAL_TICK: Color32 = Color32::from_rgb(0x5A, 0x63, 0x74);
const BORDER_COLOR: Color32 = Color32::from_rgb(0x26, 0x2B, 0x35);
const CENTER_TICK: Color32 = Color32::from_rgba_premultiplied(0x9E, 0x7B, 0x56, 0xB0);

/// Draw a vertical evaluation bar of height `side`.
///
/// White fills from the bottom upward regardless of board orientation.
pub fn draw(ui: &mut Ui, eval: Option<BoardEval>, side: f32) -> egui::Response {
    draw_with_hold(ui, eval, false, side)
}

/// Draw the vertical evaluation bar. When `eval` is `None`, render a distinct
/// neutral grey state with muted ticks rather than a misleading 50/50 split.
pub fn draw_with_hold(
    ui: &mut Ui,
    eval: Option<BoardEval>,
    is_computing: bool,
    side: f32,
) -> egui::Response {
    let side = side.clamp(64.0, 4096.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, side), Sense::click());
    let painter = ui.painter_at(rect);

    let radius = CornerRadius::same(3);
    if let Some(eval) = eval {
        let white_fraction = (eval.score.win_percent() / 100.0).clamp(0.0, 1.0) as f32;
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
    } else {
        painter.rect_filled(rect, radius, NO_EVAL_FILL);
        // Subtle dashed ticks indicate no reliable evaluation is available yet.
        let step = 10.0;
        let mut y = rect.top() + 8.0;
        while y < rect.bottom() - 6.0 {
            painter.line_segment(
                [
                    Pos2::new(rect.left() + 3.0, y),
                    Pos2::new(rect.right() - 3.0, y),
                ],
                Stroke::new(1.0, NO_EVAL_TICK),
            );
            y += step;
        }
    }

    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, BORDER_COLOR),
        StrokeKind::Inside,
    );

    let tip = match (eval, is_computing) {
        (Some(eval), true) => format!("{} · updating…", eval.tooltip()),
        (Some(eval), false) => eval.tooltip(),
        (None, true) => "No evaluation yet — computing…".to_string(),
        (None, false) => "No reliable evaluation available for this position".to_string(),
    };
    response.on_hover_text(tip)
}
