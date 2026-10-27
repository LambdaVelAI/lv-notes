//! Core engine for lv-notes.
//!
//! This crate is shared by every client (desktop, web, iPad) and later by the
//! server. It must stay portable: no platform I/O outside clearly separated
//! modules, so it can also compile to WebAssembly. SQLite storage is behind
//! the `sqlite` feature for that reason.

pub mod auth;
pub mod ids;
pub mod myst;
pub mod op;
pub mod principal;
pub mod refusal;
pub mod sequencer;
pub mod state;
#[cfg(feature = "sqlite")]
pub mod store;
