//! Brett: native Linux chess preparation workspace.
//!
//! The library crate exists so the domain model and UI can be tested without
//! starting a window. The binary is a thin wrapper around [`app::App`].

pub mod app;
pub mod domain;
pub mod library;
pub mod ui;
