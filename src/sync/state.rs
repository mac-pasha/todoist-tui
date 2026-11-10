//! Sync engine state events and status holder (T-502, EPIC-06).
//!
//! Pure data and state-transition rules for reporting background sync
//! progress to the UI (EPIC-07). Free of `tokio` and I/O so transitions and
//! duplicate-event suppression can be unit-tested deterministically.
//!
//! [`SyncState::from_error`] formats [`SyncError`] exclusively through its
//! [`std::fmt::Display`] implementation so no secret or credential material
//! can ever reach the UI status bar.

#![allow(dead_code)]

use std::time::SystemTime;

use crate::error::SyncError;

/// Wall-clock timestamp type carried by [`SyncState::Synced`].
///
/// Uses [`std::time::SystemTime`] so the crate pulls in no external date/time
/// dependency (matching `src/api/model/common.rs` and `src/db/meta.rs`).
pub type SyncTimestamp = SystemTime;

/// High-level sync lifecycle event emitted by the sync engine for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncState {
    /// A sync cycle is currently in flight.
    Syncing,
    /// The most recent sync cycle completed successfully at `at`.
    Synced {
        /// Wall-clock time at which the sync finished.
        at: SyncTimestamp,
    },
    /// The client cannot reach the Todoist API (transport-class failure).
    Offline,
    /// Local write commands are queued and waiting to be flushed.
    Pending {
        /// Number of pending commands in the offline write queue.
        count: usize,
    },
    /// A non-transport sync failure occurred.
    Error {
        /// Human-readable diagnostic derived from [`SyncError`]'s `Display`
        /// implementation; never contains secret material.
        msg: String,
    },
}

impl SyncState {
    /// Builds [`SyncState::Error`] from a [`SyncError`] using its
    /// [`std::fmt::Display`] text so raw secret material can never reach the
    /// UI.
    #[must_use]
    pub fn from_error(err: &SyncError) -> Self {
        Self::Error {
            msg: err.to_string(),
        }
    }
}

/// Holder for the latest [`SyncState`] and the current pending-command count.
///
/// Enforces two transition invariants required by T-502:
/// 1. **Duplicate-event suppression**: [`SyncStatus::apply`] returns `false`
///    when the incoming state is identical to the current state.
/// 2. **Preserve `Synced` on zero pending**: applying
///    `SyncState::Pending { count: 0 }` while the current state is
///    `SyncState::Synced { .. }` updates [`SyncStatus::pending_count`] to `0`
///    without overwriting the `Synced` state, and returns `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    state: SyncState,
    pending_count: usize,
}

impl SyncStatus {
    /// Creates a new [`SyncStatus`] starting in `initial`.
    ///
    /// If `initial` is [`SyncState::Pending`], `pending_count` is initialised
    /// to that variant's `count`; otherwise it starts at `0`.
    #[must_use]
    pub const fn new(initial: SyncState) -> Self {
        let pending_count = match &initial {
            SyncState::Pending { count } => *count,
            SyncState::Syncing
            | SyncState::Synced { .. }
            | SyncState::Offline
            | SyncState::Error { .. } => 0,
        };
        Self {
            state: initial,
            pending_count,
        }
    }

    /// Returns a reference to the current [`SyncState`].
    #[must_use]
    pub const fn state(&self) -> &SyncState {
        &self.state
    }

    /// Returns the most recently reported number of pending write commands.
    #[must_use]
    pub const fn pending_count(&self) -> usize {
        self.pending_count
    }

    /// Explicitly updates the tracked pending-command count without changing
    /// the current [`SyncState`].
    pub fn set_pending_count(&mut self, count: usize) {
        self.pending_count = count;
    }

    /// Applies `next` to this holder, returning `true` if the visible
    /// [`SyncState`] changed and `false` if it was suppressed.
    ///
    /// * Whenever `next` is `SyncState::Pending { count }`,
    ///   [`SyncStatus::pending_count`] is updated to `count`.
    /// * If `next` is `SyncState::Pending { count: 0 }` and the current state
    ///   is `SyncState::Synced { .. }`, the `Synced` state is preserved and
    ///   `false` is returned.
    /// * If `next` equals the current state, `false` is returned
    ///   (duplicate-event suppression).
    pub fn apply(&mut self, next: SyncState) -> bool {
        if let SyncState::Pending { count } = next {
            self.pending_count = count;
            if count == 0 && matches!(self.state, SyncState::Synced { .. }) {
                return false;
            }
        }

        if self.state == next {
            return false;
        }

        self.state = next;
        true
    }
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::new(SyncState::Offline)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::error::ApiError;

    /// Fake token fixture used to verify error formatting never leaks secrets.
    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn sample_time(secs: u64) -> SyncTimestamp {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn sync_state_variants_derive_debug_clone_and_partial_eq() {
        let t = sample_time(1_700_000_000);
        let states = [
            SyncState::Syncing,
            SyncState::Synced { at: t },
            SyncState::Offline,
            SyncState::Pending { count: 4 },
            SyncState::Error {
                msg: "server error".to_string(),
            },
        ];

        for state in &states {
            let cloned = state.clone();
            assert_eq!(state, &cloned);
            assert_ne!(format!("{state:?}"), "");
        }
    }

    #[test]
    fn from_error_uses_display_text_and_never_leaks_fixture_token() {
        let variants = [
            SyncError::Api {
                source: ApiError::Unauthorized,
            },
            SyncError::Api {
                source: ApiError::Server { status: 503 },
            },
            SyncError::Api {
                source: ApiError::RateLimited {
                    retry_after: Some(Duration::from_secs(5)),
                },
            },
            SyncError::Api {
                source: ApiError::InsecureBaseUrl,
            },
            SyncError::Api {
                source: ApiError::RetriesExhausted { attempts: 3 },
            },
            SyncError::Store {
                message: "sqlite busy".to_string(),
            },
            SyncError::Queue {
                message: "queue locked".to_string(),
            },
            SyncError::Offline,
            SyncError::ChannelClosed,
            SyncError::Cancelled,
        ];

        for err in &variants {
            let state = SyncState::from_error(err);
            let expected_display = err.to_string();
            match &state {
                SyncState::Error { msg } => {
                    assert_eq!(msg, &expected_display);
                    assert!(
                        !msg.contains(FIXTURE_TOKEN),
                        "SyncState::Error message leaked fixture token: {msg}"
                    );
                }
                other => panic!("expected SyncState::Error, got {other:?}"),
            }
            let debug_repr = format!("{state:?}");
            assert!(
                !debug_repr.contains(FIXTURE_TOKEN),
                "SyncState Debug leaked fixture token: {debug_repr}"
            );
        }
    }

    #[test]
    fn apply_suppresses_duplicate_states_across_all_variants() {
        let t = sample_time(1_700_000_100);
        let mut status = SyncStatus::new(SyncState::Offline);

        // Initial state is already Offline -> duplicate returns false.
        assert!(!status.apply(SyncState::Offline));

        // Transition to Syncing -> true on first, false on duplicate.
        assert!(status.apply(SyncState::Syncing));
        assert_eq!(status.state(), &SyncState::Syncing);
        assert!(!status.apply(SyncState::Syncing));

        // Transition to Synced { at } -> true on first, false on same timestamp.
        assert!(status.apply(SyncState::Synced { at: t }));
        assert_eq!(status.state(), &SyncState::Synced { at: t });
        assert!(!status.apply(SyncState::Synced { at: t }));

        // Different Synced timestamp is not a duplicate -> returns true.
        let t2 = sample_time(1_700_000_200);
        assert!(status.apply(SyncState::Synced { at: t2 }));
        assert_eq!(status.state(), &SyncState::Synced { at: t2 });

        // Transition to Pending { count: 2 } -> true on first, false on duplicate.
        assert!(status.apply(SyncState::Pending { count: 2 }));
        assert_eq!(status.state(), &SyncState::Pending { count: 2 });
        assert_eq!(status.pending_count(), 2);
        assert!(!status.apply(SyncState::Pending { count: 2 }));

        // Transition to Error -> true on first, false on duplicate.
        let err_state = SyncState::from_error(&SyncError::ChannelClosed);
        assert!(status.apply(err_state.clone()));
        assert_eq!(status.state(), &err_state);
        assert!(!status.apply(err_state));
    }

    #[test]
    fn pending_zero_while_synced_does_not_overwrite_synced_state() {
        let t = sample_time(1_700_000_500);
        let mut status = SyncStatus::new(SyncState::Pending { count: 3 });
        assert_eq!(status.pending_count(), 3);

        // Sync completes -> state becomes Synced.
        assert!(status.apply(SyncState::Synced { at: t }));
        assert_eq!(status.state(), &SyncState::Synced { at: t });

        // Queue reports 0 pending commands -> pending_count drops to 0, but
        // Synced { at } is NOT overwritten and apply returns false.
        assert!(!status.apply(SyncState::Pending { count: 0 }));
        assert_eq!(status.state(), &SyncState::Synced { at: t });
        assert_eq!(status.pending_count(), 0);
    }

    #[test]
    fn pending_non_zero_while_synced_transitions_to_pending_and_updates_count() {
        let t = sample_time(1_700_000_600);
        let mut status = SyncStatus::new(SyncState::Synced { at: t });
        assert_eq!(status.pending_count(), 0);

        assert!(status.apply(SyncState::Pending { count: 5 }));
        assert_eq!(status.state(), &SyncState::Pending { count: 5 });
        assert_eq!(status.pending_count(), 5);
    }

    #[test]
    fn pending_zero_while_in_pending_state_transitions_and_updates_count() {
        let mut status = SyncStatus::new(SyncState::Pending { count: 2 });
        assert_eq!(status.pending_count(), 2);

        assert!(status.apply(SyncState::Pending { count: 0 }));
        assert_eq!(status.state(), &SyncState::Pending { count: 0 });
        assert_eq!(status.pending_count(), 0);

        // Second Pending { count: 0 } is suppressed as a duplicate.
        assert!(!status.apply(SyncState::Pending { count: 0 }));
    }

    #[test]
    fn default_starts_offline_with_zero_pending() {
        let mut status = SyncStatus::default();
        assert_eq!(status.state(), &SyncState::Offline);
        assert_eq!(status.pending_count(), 0);

        status.set_pending_count(7);
        assert_eq!(status.pending_count(), 7);
        assert_eq!(status.state(), &SyncState::Offline);
    }
}
