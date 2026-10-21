use egui::{Color32, Rect, Sense, Ui, Vec2, pos2};
use shakmaty::{Chess, Color, File, Position, Rank, Square};

use crate::ui::pieces::PieceTextures;

const LIGHT: Color32 = Color32::from_rgb(0xEB, 0xEC, 0xD0);
const DARK: Color32 = Color32::from_rgb(0x77, 0x96, 0x56);
const LAST_MOVE: Color32 = Color32::from_rgba_premultiplied(0x58, 0x54, 0x15, 0x70);
const SELECTED: Color32 = Color32::from_rgba_premultiplied(0x12, 0x51, 0x24, 0x90);
const TARGET: Color32 = Color32::from_rgba_premultiplied(0x06, 0x13, 0x06, 0x66);

pub struct BoardState<'a> {
    pub pos: &'a Chess,
    pub orientation: Color,
    pub last_move: Option<(Square, Square)>,
    pub selected: Option<Square>,
    pub targets: &'a [Square],
}

/// Draw an interactive board. Returns the square that was clicked, if any.
pub fn draw(ui: &mut Ui, tex: &PieceTextures, st: BoardState<'_>, side: f32) -> Option<Square> {
    let side = side.clamp(64.0, 4096.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
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
            painter.circle_stroke(
                r.center(),
                cell * 0.42,
                egui::Stroke::new(cell * 0.07, TARGET),
            );
        } else {
            painter.circle_filled(r.center(), cell * 0.14, TARGET);
        }
    }

    for row in 0..8 {
        for col in 0..8 {
            let sq = square_at(col, row, st.orientation);
            if let Some(piece) = st.pos.board().piece_at(sq)
                && let Some(texture) = tex.get(piece)
            {
                let r = square_rect(sq).shrink(cell * 0.04);
                painter.image(
                    texture.id(),
                    r,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
    }

    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let col = ((p.x - rect.left()) / cell).floor();
        let row = ((p.y - rect.top()) / cell).floor();
        if (0.0..8.0).contains(&col) && (0.0..8.0).contains(&row) {
            return Some(square_at(col as i32, row as i32, st.orientation));
        }
    }

    None
}

fn is_dark(sq: Square) -> bool {
    (sq.file().to_u32() + sq.rank().to_u32()).is_multiple_of(2)
}

fn contrast_text(dark_square: bool) -> Color32 {
    if dark_square { LIGHT } else { DARK }
}

fn coords(sq: Square, orientation: Color) -> (i32, i32) {
    let file = sq.file().to_u32() as i32;
    let rank = sq.rank().to_u32() as i32;
    match orientation {
        Color::White => (file, 7 - rank),
        Color::Black => (7 - file, rank),
    }
}

fn square_at(col: i32, row: i32, orientation: Color) -> Square {
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
    fn a1_is_dark_and_h1_is_light() {
        assert!(is_dark(Square::A1));
        assert!(!is_dark(Square::H1));
        assert!(!is_dark(Square::A8));
        assert!(is_dark(Square::H8));
    }
}
