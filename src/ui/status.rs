//! UI status bar state model and `SyncState` event adapter (`T-611`, EPIC-07).

#![allow(dead_code)]

use std::time::SystemTime;

use crate::sync::state::SyncState as EngineSyncState;

/// Five visual sync states displayed in the status bar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SyncEvent {
    /// Background sync engine has synchronized cleanly.
    Synced {
        /// Timestamp of completion (optional; uses provided clock if None).
        at: Option<SystemTime>,
    },
    /// A sync cycle is actively running.
    Syncing,
    /// Client is unreachable / offline; local edits remain active.
    #[default]
    Offline,
    /// Local queued mutations waiting to flush.
    Pending {
        /// Number of pending commands.
        count: usize,
    },
    /// Non-transport sync failure.
    Error {
        /// Display message (never populated with raw secret material).
        message: String,
    },
}

impl From<EngineSyncState> for SyncEvent {
    fn from(state: EngineSyncState) -> Self {
        match state {
            EngineSyncState::Syncing => Self::Syncing,
            EngineSyncState::Synced { at } => Self::Synced { at: Some(at) },
            EngineSyncState::Offline => Self::Offline,
            EngineSyncState::Pending { count } => Self::Pending { count },
            EngineSyncState::Error { msg } => Self::Error { message: msg },
        }
    }
}

/// Model backing the bottom status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    /// Current display state.
    pub state: SyncEvent,
    /// Timestamp of the last completed sync, if any.
    pub last_sync: Option<SystemTime>,
    /// Number of pending mutations.
    pub pending_count: usize,
    /// Number of failed commands in `commands` (`T-711`).
    pub failed_count: usize,
    /// Optional status notice / search indicator.
    pub notice: Option<String>,
}

impl Default for StatusModel {
    fn default() -> Self {
        Self {
            state: SyncEvent::Offline,
            last_sync: None,
            pending_count: 0,
            failed_count: 0,
            notice: None,
        }
    }
}

impl StatusModel {
    /// Creates a new [`StatusModel`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates state from an incoming [`SyncEvent`].
    pub fn apply(&mut self, event: SyncEvent, now: SystemTime) {
        match &event {
            SyncEvent::Synced { at } => {
                self.last_sync = Some(at.unwrap_or(now));
                self.state = SyncEvent::Synced { at: self.last_sync };
            }
            SyncEvent::Pending { count } => {
                self.pending_count = *count;
                if *count > 0 {
                    self.state = event;
                }
            }
            _ => {
                self.state = event;
            }
        }
    }

    /// Formats the relative time since `last_sync` against injected `now`.
    #[must_use]
    pub fn relative_last_sync(&self, now: SystemTime) -> String {
        format_relative_time(self.last_sync, now)
    }
}

/// Formats `last_sync` relative to `now` (`"never"`, `"just now"`, `"3m ago"`, `"2h ago"`).
#[must_use]
pub fn format_relative_time(last_sync: Option<SystemTime>, now: SystemTime) -> String {
    let Some(then) = last_sync else {
        return "never".to_string();
    };
    let Ok(elapsed) = now.duration_since(then) else {
        return "just now".to_string();
    };
    let secs = elapsed.as_secs();
    if secs < 10 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn applies_all_five_states_and_updates_last_sync() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut model = StatusModel::new();
        assert_eq!(model.relative_last_sync(t0), "never");

        model.apply(SyncEvent::Syncing, t0);
        assert_eq!(model.state, SyncEvent::Syncing);

        model.apply(SyncEvent::Synced { at: Some(t0) }, t0);
        assert_eq!(model.last_sync, Some(t0));
        assert_eq!(model.relative_last_sync(t0), "just now");

        let t_3m = t0 + Duration::from_secs(180);
        assert_eq!(model.relative_last_sync(t_3m), "3m ago");

        model.apply(SyncEvent::Pending { count: 4 }, t_3m);
        assert_eq!(model.pending_count, 4);

        model.apply(SyncEvent::Offline, t_3m);
        assert_eq!(model.state, SyncEvent::Offline);

        model.apply(
            SyncEvent::Error {
                message: "server 500".to_string(),
            },
            t_3m,
        );
        assert!(matches!(model.state, SyncEvent::Error { .. }));
    }
}
