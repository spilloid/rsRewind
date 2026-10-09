//! The rsRewind window: Iced for the application chrome, a custom wgpu viewport for the rewind
//! room (screenshots as GPU textures on a 2.5D timeline, time running into depth, one lane per
//! source).
//!
//! Runs as its own process (`rsrewind ui`) and reads history only through `rsrewind-query`'s
//! [`rsrewind_query::History`] facade on a background thread. It depends on nothing that captures,
//! stores or recognizes anything, so it can crash, hang or be closed without touching recording
//! (CLAUDE.md, enforced by `rsrewind-query/tests/ui_boundary.rs`).

mod app;
mod gaps;
mod style;
mod thumb;
mod timeline;
mod viewer;
mod worker;

use rsrewind_core::DataDir;

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// Follow the operating system, and change with it.
    #[default]
    System,
    Light,
    Dark,
}

/// Opens the window on the history in `data`, following the system's appearance, and returns
/// when it is closed.
pub fn run(data: DataDir) -> anyhow::Result<()> {
    run_with(data, Appearance::System)
}

/// [`run`] with the appearance chosen.
pub fn run_with(data: DataDir, appearance: Appearance) -> anyhow::Result<()> {
    tracing::info!(?appearance, "opening the rsRewind window");
    app::run(data, appearance)
        .map_err(|error| anyhow::anyhow!("the rsRewind window failed: {error}"))
}
