//! Brett: native Linux chess preparation workspace.
//!
//! The library crate exists so the domain model and UI can be tested without
//! starting a window. The binary is a thin wrapper around [`app::App`].

pub mod app;
pub mod domain;
pub mod engine;
pub mod library;
pub mod online;
pub mod players;
pub mod position;
pub mod review;
pub mod scid;
pub mod ui;
