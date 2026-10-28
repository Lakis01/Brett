use std::collections::HashMap;

use crate::chess::{Color, Piece, Role};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};

/// Textures for the twelve pieces, loaded once per `egui::Context`.
pub struct PieceTextures {
    map: HashMap<(Color, Role), TextureHandle>,
}

impl PieceTextures {
    pub fn load(ctx: &Context) -> Self {
        let mut map = HashMap::with_capacity(12);
        for color in [Color::White, Color::Black] {
            for role in [
                Role::King,
                Role::Queen,
                Role::Rook,
                Role::Bishop,
                Role::Knight,
                Role::Pawn,
            ] {
                let key = match (color, role) {
                    (Color::White, Role::King) => "wK",
                    (Color::White, Role::Queen) => "wQ",
                    (Color::White, Role::Rook) => "wR",
                    (Color::White, Role::Bishop) => "wB",
                    (Color::White, Role::Knight) => "wN",
                    (Color::White, Role::Pawn) => "wP",
                    (Color::Black, Role::King) => "bK",
                    (Color::Black, Role::Queen) => "bQ",
                    (Color::Black, Role::Rook) => "bR",
                    (Color::Black, Role::Bishop) => "bB",
                    (Color::Black, Role::Knight) => "bN",
                    (Color::Black, Role::Pawn) => "bP",
                };
                map.insert((color, role), load_png(ctx, key));
            }
        }
        Self { map }
    }

    pub fn get(&self, piece: Piece) -> Option<&TextureHandle> {
        self.map.get(&(piece.color, piece.role))
    }
}

fn load_png(ctx: &Context, key: &str) -> TextureHandle {
    let bytes: &[u8] = match key {
        "wK" => include_bytes!("../../assets/pieces/wK.png"),
        "wQ" => include_bytes!("../../assets/pieces/wQ.png"),
        "wR" => include_bytes!("../../assets/pieces/wR.png"),
        "wB" => include_bytes!("../../assets/pieces/wB.png"),
        "wN" => include_bytes!("../../assets/pieces/wN.png"),
        "wP" => include_bytes!("../../assets/pieces/wP.png"),
        "bK" => include_bytes!("../../assets/pieces/bK.png"),
        "bQ" => include_bytes!("../../assets/pieces/bQ.png"),
        "bR" => include_bytes!("../../assets/pieces/bR.png"),
        "bB" => include_bytes!("../../assets/pieces/bB.png"),
        "bN" => include_bytes!("../../assets/pieces/bN.png"),
        "bP" => include_bytes!("../../assets/pieces/bP.png"),
        _ => unreachable!("unknown piece key"),
    };

    let image = image::load_from_memory(bytes)
        .expect("embedded piece PNG must decode")
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = image.into_raw();
    let color_image = ColorImage::from_rgba_unmultiplied(size, &pixels);
    ctx.load_texture(format!("piece-{key}"), color_image, TextureOptions::LINEAR)
}
