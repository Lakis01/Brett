//! Brett-owned chess rules facade backed by the MIT-licensed `cozy-chess`
//! crate. This module is the only boundary between Brett and a chess engine
//! library.

pub mod board;

// The brief names this file `chess.rs` inside `src/chess/`: `chess::chess` is
// required, not accidental.
#[allow(clippy::module_inception)]
pub mod chess;
pub mod fen;
pub mod r#move;
pub mod san;
pub mod types;

pub use board::*;
pub use chess::*;
pub use fen::*;
pub use r#move::*;
pub use san::*;
pub use types::*;
