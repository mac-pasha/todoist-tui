//! Terminal UI scaffold (T-601, EPIC-07).
//!
//! This task only lands the error vocabulary (`UiError` in `src/error.rs`)
//! and this module scaffold; the ratatui render loop, input handling and
//! keymap wiring arrive in later EPIC-07 tasks so the rest of the epic can
//! land in parallel.
//!
//! [`actions`] lands the write-path action vocabulary (`TaskAction` and its
//! `ActionError`, T-701, EPIC-08): the enum/payload types only, ahead of
//! the dispatch/queueing wiring that consumes them in later EPIC-08 tasks.

// No consumers yet: later EPIC-07 tasks add the terminal setup/teardown,
// render loop and input handling here. `pub` alone does not silence
// `dead_code` in a binary crate, so allow it at module scope — same pattern
// as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod actions;
pub mod app;
pub mod data;
pub mod filters_view;
pub mod keymap;
pub mod model;
pub mod run;
pub mod search;
pub mod session;
pub mod status;
pub mod structure;
pub mod terminal;
pub mod theme;
pub mod view;
pub mod widgets;

pub use app::{App, Pane};
pub use data::{FixtureData, SqliteViewData, ViewData};
pub use keymap::{parse_key, render_key, Action, KeyBinding, Keymap};
pub use model::{
    Comment, SectionHeader, SidebarKind, SidebarNode, SidebarNodeKind, TaskDetail, TaskRow, ViewId,
};
pub use status::{StatusModel, SyncEvent};
pub use terminal::TerminalGuard;
pub use theme::{ColorDepth, Theme};
