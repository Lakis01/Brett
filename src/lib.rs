//! Brett: native Linux chess preparation workspace.
//!
//! The library crate exists so the domain model and UI can be tested without
//! starting a window. The binary is a thin wrapper around [`app::App`].

pub mod analysis_cache;
pub mod app;
pub mod book;
pub mod cbh;
pub mod cbv;
pub mod champions;
pub mod chess;
pub mod cloud;
pub mod country;
pub mod domain;
pub mod eco;
pub mod engine;
pub mod eval;
pub mod evaluation;
pub mod fide;
pub mod library;
pub mod online;
pub mod pgn;
pub mod photos;
pub mod players;
pub mod position;
pub mod print;
pub mod recent;
pub mod review;
pub mod review_book;
pub mod scid;
pub mod session;
pub mod settings;
pub mod setup;
#[doc(hidden)]
pub mod test_data;
pub mod titles;
pub mod twocbh;
pub mod ui;
pub mod wiki;
