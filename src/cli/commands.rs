//! Subcommand dispatch (T-007, EPIC-01; auth wired in T-107).
//!
//! Each variant of [`crate::cli::Commands`] routes to its implementation:
//! `auth` is real (T-107: prompt / validate / store, see [`crate::cli::auth`]),
//! while the others still return [`AppError::NotImplemented`] carrying their
//! name until their epics land (logout: EPIC-02 keychain; sync: EPIC-02/03;
//! add: EPIC-11 quick-add parsing; today: EPIC-05 task list). Kept free of
//! network, storage and UI code on purpose — those live behind the routed
//! modules.

use crate::cli::auth::run_auth;
use crate::cli::Commands;
use crate::error::{AppError, Result};

/// Routes a parsed subcommand to its implementation.
///
/// # Errors
///
/// Returns whatever the routed implementation returns for `auth`; the
/// remaining stubs return [`AppError::NotImplemented`] with the
/// subcommand's name.
pub fn dispatch(command: &Commands) -> Result<()> {
    match command {
        Commands::Auth => run_auth(),
        Commands::Logout => Err(AppError::NotImplemented("logout")),
        Commands::Sync => Err(AppError::NotImplemented("sync")),
        // `text` is intentionally unused: this task only captures the quick-add
        // argument on the CLI surface, EPIC-11 parses it.
        Commands::Add { text: _ } => Err(AppError::NotImplemented("add")),
        Commands::Today { json: _ } => Err(AppError::NotImplemented("today")),
    }
}
