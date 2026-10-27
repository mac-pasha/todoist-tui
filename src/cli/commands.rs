//! Subcommand stubs (T-007, EPIC-01).
//!
//! Every variant of [`crate::cli::Commands`] maps to an
//! [`AppError::NotImplemented`] carrying its name; later tasks replace each
//! arm with the real behaviour (auth/logout: EPIC-02 keychain; sync:
//! EPIC-02/03; add: EPIC-11 quick-add parsing; today: EPIC-05 task list).
//! Kept free of network, storage and UI code on purpose — those land with
//! the epics named above.

use crate::cli::Commands;
use crate::error::{AppError, Result};

/// Routes a parsed subcommand to its stub.
///
/// # Errors
///
/// Always returns [`AppError::NotImplemented`] with the subcommand's name in
/// this task.
pub fn dispatch(command: &Commands) -> Result<()> {
    match command {
        Commands::Auth => Err(AppError::NotImplemented("auth")),
        Commands::Logout => Err(AppError::NotImplemented("logout")),
        Commands::Sync => Err(AppError::NotImplemented("sync")),
        // `text` is intentionally unused: this task only captures the quick-add
        // argument on the CLI surface, EPIC-11 parses it.
        Commands::Add { text: _ } => Err(AppError::NotImplemented("add")),
        Commands::Today { json: _ } => Err(AppError::NotImplemented("today")),
    }
}
