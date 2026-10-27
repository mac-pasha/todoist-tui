//! Configuration subsystem (EPIC-01).
//!
//! T-004 lands platform directory resolution in [`paths`]; T-005 layers the
//! TOML config model and loader on top of it (`model.rs`, `load.rs`).

pub mod paths;
