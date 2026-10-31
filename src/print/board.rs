//! Self-contained SVG chess-board diagrams.
//!
//! Each diagram is a single SVG document with an explicit 420 × 420 viewBox so
//! Typst and LaTeX can embed the same asset without depending on fonts for the
//! board itself. The twelve cburnett piece files are compiled into the binary
//! and their inner artwork is copied into the diagram.

use crate::chess::{Chess, Color, File, Piece, Position, Rank, Role, Square};

const MARGIN: i32 = 24;
const CELL: i32 = 45;
const LIGHT: &str = "#EBECD0";
const DARK: &str = "#779656";
const TEXT: &str = "#333333";

const ATTRIBUTION: &str = "<!-- Pieces: cburnett by Colin M. L. Burnett, CC BY-SA 3.0, https://creativecommons.org/licenses/by-sa/3.0/ -->";

/// Render one position as a standalone SVG diagram.
pub(super) fn board_svg(pos: &Chess) -> String {
    let mut svg = String::with_capacity(8 * 1024);
    svg.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 420 420\" width=\"420\" height=\"420\" role=\"img\" aria-label=\"Chess diagram\">\n",
    );
    svg.push_str(ATTRIBUTION);
    svg.push('\n');

    for rank in 0..8i32 {
        for file in 0..8i32 {
            let dark = (file + rank) % 2 == 0;
            let x = MARGIN + file * CELL;
            let y = MARGIN + (7 - rank) * CELL;
            svg.push_str(&format!(
                "<rect class=\"square\" x=\"{x}\" y=\"{y}\" width=\"{CELL}\" height=\"{CELL}\" fill=\"{}\"/>\n",
                if dark { DARK } else { LIGHT }
            ));
        }
    }

    for file in 0..8 {
        let x = MARGIN + file * CELL + CELL / 2;
        let letter = (b'a' + file as u8) as char;
        for y in [16, 410] {
            svg.push_str(&format!(
                "<text class=\"coordinate\" x=\"{x}\" y=\"{y}\" dy=\".35em\" text-anchor=\"middle\" font-family=\"DejaVu Sans, Liberation Sans, sans-serif\" font-size=\"12\" fill=\"{TEXT}\">{letter}</text>\n"
            ));
        }
    }

    for rank in 0..8 {
        let y = MARGIN + (7 - rank) * CELL + CELL / 2;
        let number = rank + 1;
        for x in [12, 402] {
            svg.push_str(&format!(
                "<text class=\"coordinate\" x=\"{x}\" y=\"{y}\" dy=\".35em\" text-anchor=\"middle\" font-family=\"DejaVu Sans, Liberation Sans, sans-serif\" font-size=\"12\" fill=\"{TEXT}\">{number}</text>\n"
            ));
        }
    }

    for rank in 0..8u32 {
        for file in 0..8u32 {
            let square = Square::from_coords(File::new(file), Rank::new(rank));
            let Some(piece) = pos.board().piece_at(square) else {
                continue;
            };
            let x = MARGIN + file as i32 * CELL;
            let y = MARGIN + (7 - rank as i32) * CELL;
            let (name, markup) = piece_assets(piece);
            svg.push_str(&format!(
                "<g class=\"piece\" data-piece=\"{name}\" transform=\"translate({x} {y})\">"
            ));
            svg.push_str(piece_inner(markup));
            svg.push_str("</g>\n");
        }
    }

    let (side, cy) = match pos.turn() {
        Color::White => ("white", MARGIN + 7 * CELL + CELL / 2),
        Color::Black => ("black", MARGIN + CELL / 2),
    };
    svg.push_str(&format!(
        "<circle class=\"side-to-move\" data-side-to-move=\"{side}\" cx=\"414\" cy=\"{cy}\" r=\"5\" fill=\"{TEXT}\"/>\n"
    ));
    svg.push_str("</svg>\n");
    svg
}

fn piece_assets(piece: Piece) -> (&'static str, &'static str) {
    match (piece.color, piece.role) {
        (Color::White, Role::King) => ("wK", include_str!("../../assets/pieces/wK.svg")),
        (Color::White, Role::Queen) => ("wQ", include_str!("../../assets/pieces/wQ.svg")),
        (Color::White, Role::Rook) => ("wR", include_str!("../../assets/pieces/wR.svg")),
        (Color::White, Role::Bishop) => ("wB", include_str!("../../assets/pieces/wB.svg")),
        (Color::White, Role::Knight) => ("wN", include_str!("../../assets/pieces/wN.svg")),
        (Color::White, Role::Pawn) => ("wP", include_str!("../../assets/pieces/wP.svg")),
        (Color::Black, Role::King) => ("bK", include_str!("../../assets/pieces/bK.svg")),
        (Color::Black, Role::Queen) => ("bQ", include_str!("../../assets/pieces/bQ.svg")),
        (Color::Black, Role::Rook) => ("bR", include_str!("../../assets/pieces/bR.svg")),
        (Color::Black, Role::Bishop) => ("bB", include_str!("../../assets/pieces/bB.svg")),
        (Color::Black, Role::Knight) => ("bN", include_str!("../../assets/pieces/bN.svg")),
        (Color::Black, Role::Pawn) => ("bP", include_str!("../../assets/pieces/bP.svg")),
    }
}

/// Strip the outer `<svg>` element from a cburnett file, keeping its artwork.
fn piece_inner(markup: &str) -> &str {
    let start = markup.find('>').map_or(0, |index| index + 1);
    let end = markup.rfind("</svg>").unwrap_or(markup.len());
    markup[start..end].trim()
}
