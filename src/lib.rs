//! Brett: native Linux chess preparation workspace.
//!
//! The library crate exists so the domain model and UI can be tested without
//! starting a window. The binary is a thin wrapper around [`app::App`].

pub mod app;
pub mod cbh;
pub mod cloud;
pub mod domain;
pub mod engine;
pub mod evaluation;
pub mod library;
pub mod online;
pub mod pgn;
pub mod players;
pub mod position;
pub mod review;
pub mod scid;
pub mod session;
pub mod settings;
pub mod ui;
