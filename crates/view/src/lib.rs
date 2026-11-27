//! Maestro's owner surface: the generative-UI catalog ([`catalog`]), maud-rendered views read
//! from a fresh Store ([`render`]), and the MCP App page with its pinned bridge ([`page`]).
//!
//! The agent composes; Rust decides what is shown. A spec names block types and ledger handles,
//! [`catalog::validate`] places what it can and records every drop, and [`render::render`] reads
//! every value from the Store it is given. The MCP layer builds that Store per call.

pub mod catalog;
pub mod flows;
pub mod handoff;
pub mod page;
pub mod render;
mod screens;

pub use catalog::{Drop, Validated, catalog, validate};
pub use render::{Rendered, render, render_app, render_at, render_web, render_with};
pub use screens::flows_web::{FlowsPage, flows_page};

/// The Decisions page for an existing ask, with both locator pieces confined to path segments.
pub fn decision_path(ledger: &str, id: &str) -> Option<String> {
    if [ledger, id].iter().any(|s| s.is_empty() || matches!(*s, "." | "..") || s.contains(['/', '\\'])) {
        return None;
    }
    Some(format!("/decisions/{}/{}", screens::web::segment(ledger), screens::web::segment(id)))
}
