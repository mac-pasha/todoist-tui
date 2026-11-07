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

#[cfg(test)]
mod tests {
    //! Unit tests for the T-403 queued command model (EPIC-05).
    //!
    //! Written by the independent unit-test pass, not the implementation
    //! author: every T-403 acceptance item maps to at least one test below,
    //! plus edge/negative cases. All ids, temp ids, names and uuids are
    //! fake fixture data — no real Todoist ids or personal data appear.
    use super::*;
    use std::str::FromStr;

    // ---------------------------------------------------------------
    // CommandState: as_str / FromStr round-trips + unknown strings.
    // ---------------------------------------------------------------

    #[test]
    fn command_state_as_str_matches_wire_names() {
        assert_eq!(CommandState::Pending.as_str(), "pending");
        assert_eq!(CommandState::InFlight.as_str(), "in_flight");
        assert_eq!(CommandState::Failed.as_str(), "failed");
    }

    #[test]
    fn command_state_display_delegates_to_as_str() {
        for state in [
            CommandState::Pending,
            CommandState::InFlight,
            CommandState::Failed,
        ] {
            assert_eq!(state.to_string(), state.as_str());
        }
    }

    #[test]
    fn command_state_from_str_parses_all_wire_names() {
        assert_eq!(
            CommandState::from_str("pending").expect("parse pending"),
            CommandState::Pending
        );
        assert_eq!(
            CommandState::from_str("in_flight").expect("parse in_flight"),
            CommandState::InFlight
        );
        assert_eq!(
            CommandState::from_str("failed").expect("parse failed"),
            CommandState::Failed
        );
    }

    #[test]
    fn command_state_round_trips_as_str_then_from_str() {
        for state in [
            CommandState::Pending,
            CommandState::InFlight,
            CommandState::Failed,
        ] {
            assert_eq!(
                CommandState::from_str(state.as_str()).expect("wire name parses back"),
                state,
                "as_str/from_str must be inverse for {}",
                state.as_str()
            );
        }
    }

    #[test]
    fn command_state_from_str_unknown_string_is_error_not_panic() {
        // Empty string, wrong case, near-misses and separators: none of
        // these are wire names, so all must return the documented
        // `QueueError::UnknownCommandType` carrying the offending input.
        for bogus in [
            "",
            "Pending",
            "PENDING",
            "IN_FLIGHT",
            "inflight",
            "in-flight",
            "in flight",
            "failed ",
            " failed",
            "pending/failed",
            "succeeded",
        ] {
            match CommandState::from_str(bogus) {
                Err(QueueError::UnknownCommandType { kind }) => {
                    assert_eq!(kind, bogus, "error must echo the unknown input");
                }
                other => {
                    panic!("expected QueueError::UnknownCommandType for {bogus:?}, got {other:?}")
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // QueuedCommand::new: fresh uuid per call + constructor defaults.
    // ---------------------------------------------------------------

    #[test]
    fn new_generates_a_fresh_uuid_v4_per_call() {
        let first = QueuedCommand::new("item_add", json!({ "content": "fixture" }), None);
        let second = QueuedCommand::new("item_add", json!({ "content": "fixture" }), None);
        assert_ne!(
            first.uuid, second.uuid,
            "each construction must mint its own uuid"
        );
        assert_eq!(first.uuid.get_version_num(), 4, "uuid must be v4");
        assert_eq!(second.uuid.get_version_num(), 4, "uuid must be v4");
    }

    #[test]
    fn new_applies_defaults_and_passes_fields_through() {
        let cmd = QueuedCommand::new(
            "project_delete",
            json!({ "id": "6XG7QQFxQcQQ" }),
            Some("tmp-fixture-project-delete".to_string()),
        );
        assert_eq!(cmd.kind, "project_delete");
        assert_eq!(cmd.args, json!({ "id": "6XG7QQFxQcQQ" }));
        assert_eq!(cmd.temp_id.as_deref(), Some("tmp-fixture-project-delete"));
        assert_eq!(
            cmd.state,
            CommandState::Pending,
            "new commands start Pending"
        );
        assert_eq!(cmd.attempts, 0, "new commands have zero attempts");
        assert!(cmd.error.is_none(), "new commands carry no error");
        let now = now_unix();
        assert!(
            (cmd.created_at - now).abs() <= 60,
            "created_at {} must be within a minute of unix-now {now}",
            cmd.created_at
        );
    }

    // ---------------------------------------------------------------
    // to_sync_command: exact key set, temp_id only when Some.
    // ---------------------------------------------------------------

    #[test]
    fn to_sync_command_without_temp_id_has_exactly_type_uuid_args() {
        let cmd = QueuedCommand::new("item_delete", json!({ "id": "5VmXrPrQmMvQ" }), None);
        let value = cmd.to_sync_command();
        let obj = value.as_object().expect("sync command is a JSON object");
        assert_eq!(
            obj.len(),
            3,
            "only type/uuid/args allowed; no bookkeeping fields may leak: {obj:?}"
        );
        assert_eq!(obj["type"], json!("item_delete"));
        assert_eq!(obj["args"], json!({ "id": "5VmXrPrQmMvQ" }));
        assert!(obj.contains_key("uuid"), "uuid key must be present");
        assert!(
            value.get("temp_id").is_none(),
            "temp_id key must be absent entirely (not even null) when temp_id is None: {value:?}"
        );
    }

    #[test]
    fn to_sync_command_uuid_is_the_hyphenated_command_uuid() {
        let cmd = QueuedCommand::new("item_add", json!({ "content": "fixture" }), None);
        let value = cmd.to_sync_command();
        let uuid_str = value["uuid"].as_str().expect("uuid is a JSON string");
        assert_eq!(uuid_str, cmd.uuid.to_string());
        assert_eq!(Uuid::parse_str(uuid_str), Ok(cmd.uuid));
    }

    #[test]
    fn to_sync_command_with_temp_id_matches_literal_expected_json() {
        // project_add-style command: temp id rides top-level next to the
        // envelope keys, asserted against a literal expected value.
        let cmd = QueuedCommand::new(
            "project_add",
            json!({
                "name": "Groceries (fixture)",
                "color": "berry_red",
                "child_order": 1,
            }),
            Some("tmp-fixture-project-1".to_string()),
        );
        let expected = json!({
            "type": "project_add",
            "temp_id": "tmp-fixture-project-1",
            "uuid": cmd.uuid.to_string(),
            "args": {
                "name": "Groceries (fixture)",
                "color": "berry_red",
                "child_order": 1,
            },
        });
        assert_eq!(cmd.to_sync_command(), expected);
    }

    #[test]
    fn to_sync_command_keeps_args_nested_and_untouched() {
        // Adversarial args containing keys that would collide with the
        // envelope if args were merged/flattened into the top-level object.
        let args = json!({
            "type": "not-a-command-type",
            "uuid": "not-a-command-uuid",
            "temp_id": "inside-args-not-top-level",
            "id": "pQwErTy1234",
        });
        let cmd = QueuedCommand::new(
            "item_move",
            args.clone(),
            Some("tmp-fixture-move-1".to_string()),
        );
        let value = cmd.to_sync_command();
        let obj = value.as_object().expect("sync command is a JSON object");
        assert_eq!(
            obj.len(),
            4,
            "exactly type/uuid/args/temp_id allowed: {obj:?}"
        );
        assert_eq!(obj["args"], args, "args must pass through unmodified");
        assert_eq!(obj["type"], json!("item_move"), "envelope type wins");
        assert_eq!(
            obj["uuid"],
            json!(cmd.uuid.to_string()),
            "envelope uuid wins"
        );
        assert_eq!(obj["temp_id"], json!("tmp-fixture-move-1"));
        assert_eq!(
            obj["args"]["temp_id"],
            json!("inside-args-not-top-level"),
            "args keys must not leak into the envelope"
        );
    }

    #[test]
    fn to_sync_command_is_idempotent_and_byte_identical() {
        let cmd = QueuedCommand::new(
            "item_add",
            json!({ "content": "fixture item", "project_id": "6XG7QQFxQcQQ" }),
            Some("tmp-fixture-item-9".to_string()),
        );
        let first = serde_json::to_string(&cmd.to_sync_command()).expect("serialise first");
        let second = serde_json::to_string(&cmd.to_sync_command()).expect("serialise second");
        assert_eq!(
            first, second,
            "re-serialising the same command twice must be byte-identical"
        );
        // And the uuid inside must be the construction-time one, never
        // regenerated per call (the Sync API deduplicates retries by it).
        assert_eq!(cmd.to_sync_command()["uuid"], json!(cmd.uuid.to_string()));
    }

    // ---------------------------------------------------------------
    // Edge cases.
    // ---------------------------------------------------------------

    #[test]
    fn to_sync_command_with_empty_args_is_still_valid() {
        let cmd = QueuedCommand::new("completed_get_stats", json!({}), None);
        let value = cmd.to_sync_command();
        assert_eq!(value["args"], json!({}), "empty args pass through as {{}}");
        assert_eq!(
            value.as_object().expect("command is a JSON object").len(),
            3
        );
    }

    #[test]
    fn empty_temp_id_string_is_present_and_distinct_from_none() {
        let with_empty =
            QueuedCommand::new("project_add", json!({ "name": "F" }), Some(String::new()));
        let value = with_empty.to_sync_command();
        assert_eq!(
            value["temp_id"],
            json!(""),
            "Some(\"\") must serialise as an empty string, not be dropped"
        );
        assert_eq!(
            value.as_object().expect("command is a JSON object").len(),
            4
        );

        let without = QueuedCommand::new("project_add", json!({ "name": "F" }), None);
        assert!(
            without.to_sync_command().get("temp_id").is_none(),
            "None must omit the key entirely — it is not the same as Some(\"\")"
        );
    }

    #[test]
    fn queued_command_serde_round_trip_preserves_all_fields() {
        let cmd = QueuedCommand::new(
            "item_update",
            json!({ "id": "6XG7QQFxQcQQ", "content": "renamed fixture" }),
            Some("tmp-fixture-item-update".to_string()),
        );
        let value = serde_json::to_value(&cmd).expect("serialise QueuedCommand");
        assert_eq!(
            value["uuid"],
            json!(cmd.uuid.to_string()),
            "uuid must serialise through the hyphenated string form"
        );
        let back: QueuedCommand = serde_json::from_value(value).expect("deserialise QueuedCommand");
        assert_eq!(back, cmd, "round-trip must preserve every field");
    }
}
