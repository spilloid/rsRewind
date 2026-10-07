//! The rsRewind window: Iced for the application chrome, a custom wgpu viewport for the rewind
//! room (screenshots as GPU textures on a 2.5D timeline, time running into depth, one lane per
//! source).
//!
//! Runs as its own process (`rsrewind ui`) and reads history only through `rsrewind-query`'s
//! [`rsrewind_query::History`] facade on a background thread. It depends on nothing that captures,
//! stores or recognizes anything, so it can crash, hang or be closed without touching recording
//! (CLAUDE.md, enforced by `rsrewind-query/tests/ui_boundary.rs`).

mod app;
mod style;
mod thumb;
mod timeline;
mod worker;

use rsrewind_core::DataDir;

/// Opens the window on the history in `data` and returns when it is closed.
pub fn run(data: DataDir) -> anyhow::Result<()> {
    tracing::info!("opening the rsRewind window");
    app::run(data).map_err(|error| anyhow::anyhow!("the rsRewind window failed: {error}"))
}
