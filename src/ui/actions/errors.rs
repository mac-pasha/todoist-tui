//! Failed-commands error controller (`T-711`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;
use uuid::Uuid;

use crate::error::{QueueError, Result};
use crate::queue::status::{failed_commands, FailedCommand};
use crate::queue::store::CommandStore;
use crate::ui::widgets::errors_view::ErrorsModal;

/// Opens [`ErrorsModal`] populated with all failed queue commands.
///
/// # Errors
///
/// Propagates SQLite query errors.
pub fn open_errors_modal(conn: &Connection) -> Result<ErrorsModal> {
    let items = failed_commands(conn)?;
    Ok(ErrorsModal::new(items))
}

/// Returns the current number of failed commands in `commands`.
///
/// # Errors
///
/// Propagates SQLite query errors.
pub fn failed_command_count(conn: &Connection) -> Result<usize> {
    let counts = CommandStore::new(conn).count_by_state()?;
    Ok(usize::try_from(counts.failed).unwrap_or(usize::MAX))
}

/// Dismisses (removes) a failed command entry by its UUID string.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] or parse error if the UUID string is invalid.
pub fn dismiss_failed_command(conn: &Connection, uuid_str: &str) -> Result<Vec<FailedCommand>> {
    let uuid = Uuid::parse_str(uuid_str).map_err(|_| QueueError::UnknownCommandType {
        kind: format!("invalid uuid: {uuid_str}"),
    })?;
    CommandStore::new(conn).remove(uuid)?;
    failed_commands(conn)
}
