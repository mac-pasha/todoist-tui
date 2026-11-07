//! Queued command model and its Sync API JSON projection (T-403, EPIC-05).
//!
//! [`QueuedCommand`] is the in-memory/persisted shape of a single write
//! waiting to be sent to the Todoist Sync API; [`CommandState`] tracks
//! where it is in the submit/replay lifecycle. This module only models the
//! data and its JSON projection ([`QueuedCommand::to_sync_command`]) — no
//! DB, no HTTP; those land in later EPIC-05 tasks.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::QueueError;

/// Where a [`QueuedCommand`] is in its submit/replay lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandState {
    /// Queued locally, not yet submitted to the Sync API.
    Pending,
    /// Submitted to the Sync API; awaiting a response.
    InFlight,
    /// The Sync API rejected the command (non-`ok` status).
    Failed,
}

impl CommandState {
    /// Lowercase wire representation, the inverse of [`FromStr::from_str`].
    pub fn as_str(self) -> &'static str {
        match self {
            CommandState::Pending => "pending",
            CommandState::InFlight => "in_flight",
            CommandState::Failed => "failed",
        }
    }
}

impl fmt::Display for CommandState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CommandState {
    type Err = QueueError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(CommandState::Pending),
            "in_flight" => Ok(CommandState::InFlight),
            "failed" => Ok(CommandState::Failed),
            other => Err(QueueError::UnknownCommandType {
                kind: other.to_string(),
            }),
        }
    }
}

/// A single write command waiting to be submitted to (or replayed against)
/// the Todoist Sync API.
///
/// `uuid` is generated once, at construction, and never changes for the
/// lifetime of the command: the Sync API uses it to deduplicate retried
/// submissions, so re-sending the same [`QueuedCommand`] must always carry
/// the same `uuid`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedCommand {
    /// Stable identifier for this command, generated once at construction
    /// and echoed back by the Sync API in `CommandStatus`/`CommandError`.
    ///
    /// The `uuid` crate's `v4` feature (already a dependency) does not pull
    /// in its optional `serde` feature, so this field (de)serialises
    /// through the hyphenated string form via [`uuid_as_string`] rather
    /// than deriving `Serialize`/`Deserialize` on [`Uuid`] directly.
    #[serde(with = "uuid_as_string")]
    pub uuid: Uuid,
    /// Sync API command type tag (e.g. `"item_add"`, `"project_add"`).
    pub kind: String,
    /// Command-specific arguments, passed through unmodified.
    pub args: Value,
    /// Client-generated temporary ID for commands that create a new
    /// resource (e.g. `item_add`), so later queued commands can reference
    /// the not-yet-real ID before the Sync API resolves it.
    pub temp_id: Option<String>,
    /// Where this command is in its submit/replay lifecycle.
    pub state: CommandState,
    /// Number of times this command has been submitted to the Sync API.
    pub attempts: u32,
    /// Unix timestamp (seconds) at which this command was queued.
    pub created_at: i64,
    /// Todoist's human-readable error message, set once the Sync API
    /// rejects this command (not a secret: never a token or credential).
    pub error: Option<String>,
}

impl QueuedCommand {
    /// Builds a new, freshly queued command: `uuid` is generated via
    /// [`Uuid::new_v4`] exactly once here, `state` starts at
    /// [`CommandState::Pending`], `attempts` at `0`, `error` at `None`, and
    /// `created_at` is the current wall-clock time.
    pub fn new(kind: impl Into<String>, args: Value, temp_id: Option<String>) -> Self {
        Self {
            uuid: Uuid::new_v4(),
            kind: kind.into(),
            args,
            temp_id,
            state: CommandState::Pending,
            attempts: 0,
            created_at: now_unix(),
            error: None,
        }
    }

    /// Projects this command into the JSON object shape the Sync API's
    /// `commands` array expects: `type`, `uuid`, `args`, and — only when
    /// `temp_id` is `Some` — `temp_id`. Every other field of
    /// [`QueuedCommand`] (`state`, `attempts`, `created_at`, `error`) is
    /// local bookkeeping and never reaches the wire.
    pub fn to_sync_command(&self) -> Value {
        let mut command = json!({
            "type": self.kind,
            "uuid": self.uuid.to_string(),
            "args": self.args,
        });
        if let Some(temp_id) = &self.temp_id {
            command["temp_id"] = json!(temp_id);
        }
        command
    }
}

/// Current Unix timestamp in seconds, falling back to `0` if the system
/// clock is set before the epoch (same pattern as `src/api/retry.rs` and
/// `src/db/meta.rs`).
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// (De)serialises a [`Uuid`] as its hyphenated string form.
///
/// The `uuid` crate's `serde` feature is not enabled for this crate (only
/// `v4`, for [`Uuid::new_v4`]), so [`QueuedCommand::uuid`] opts into this
/// module via `#[serde(with = "uuid_as_string")]` instead of deriving
/// `Serialize`/`Deserialize` on [`Uuid`] directly.
mod uuid_as_string {
    use super::{Deserialize, Deserializer, Serializer, Uuid};

    pub fn serialize<S>(uuid: &Uuid, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(uuid)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Uuid::parse_str(&s).map_err(serde::de::Error::custom)
    }
}
