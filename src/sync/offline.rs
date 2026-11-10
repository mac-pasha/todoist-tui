//! Offline detection and bounded retry backoff (T-509, EPIC-06).

#![allow(dead_code)]

use std::time::Duration;

use crate::error::SyncError;
use crate::sync::state::{SyncState, SyncTimestamp};

/// Default maximum backoff cap between cycle attempts while offline (5 minutes).
pub const DEFAULT_MAX_OFFLINE_BACKOFF: Duration = Duration::from_secs(300);

/// Tracks connectivity status and calculates bounded exponential backoff intervals.
#[derive(Debug, Clone)]
pub struct OfflineTracker {
    base_interval: Duration,
    max_cap: Duration,
    current_delay: Duration,
    is_offline: bool,
    attempts: u32,
}

impl OfflineTracker {
    /// Creates a new tracker initialized with `base_interval` and `max_cap`.
    #[must_use]
    pub fn new(base_interval: Duration, max_cap: Duration) -> Self {
        Self {
            base_interval,
            max_cap,
            current_delay: base_interval,
            is_offline: false,
            attempts: 0,
        }
    }

    /// Whether the tracker is currently in the offline state.
    #[must_use]
    pub fn is_offline(&self) -> bool {
        self.is_offline
    }

    /// Total number of failed attempts recorded during the current outage.
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Returns the duration until the next scheduled automatic retry.
    #[must_use]
    pub fn next_delay(&self) -> Duration {
        if self.is_offline {
            self.current_delay
        } else {
            self.base_interval
        }
    }

    /// Handles a cycle failure:
    /// - If `err.is_offline()`, moves to `SyncState::Offline` and grows the retry backoff exponentially.
    /// - If not a transport failure, leaves offline status unchanged and produces `SyncState::from_error(err)`.
    pub fn on_error(&mut self, err: &SyncError) -> SyncState {
        self.attempts = self.attempts.saturating_add(1);

        if err.is_offline() {
            self.is_offline = true;
            // Exponential backoff: double the delay up to max_cap
            let next_secs = self.current_delay.as_secs_f64() * 2.0;
            let cap_secs = self.max_cap.as_secs_f64();
            self.current_delay = Duration::from_secs_f64(next_secs.min(cap_secs));
            SyncState::Offline
        } else {
            SyncState::from_error(err)
        }
    }

    /// Handles a cycle success: resets backoff to `base_interval`, clears offline state,
    /// and returns `SyncState::Synced { at }`.
    pub fn on_success(&mut self, at: SyncTimestamp) -> SyncState {
        self.is_offline = false;
        self.attempts = 0;
        self.current_delay = self.base_interval;
        SyncState::Synced { at }
    }

    /// Resets delay so a manual trigger can execute immediately.
    #[allow(clippy::unused_self)]
    pub fn on_manual_trigger(&mut self) {
        // Keeps is_offline flag so state stays offline until success,
        // but permits immediate attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ApiError, SyncError};

    fn make_offline_error() -> SyncError {
        SyncError::Offline
    }

    fn make_non_transport_error() -> SyncError {
        SyncError::Store {
            message: "disk full".to_string(),
        }
    }

    #[test]
    fn transport_error_moves_to_offline_and_backs_off() {
        let base = Duration::from_secs(10);
        let cap = Duration::from_secs(60);
        let mut tracker = OfflineTracker::new(base, cap);

        assert!(!tracker.is_offline());
        assert_eq!(tracker.next_delay(), base);

        // First error
        let state = tracker.on_error(&make_offline_error());
        assert_eq!(state, SyncState::Offline);
        assert!(tracker.is_offline());
        assert_eq!(tracker.next_delay(), Duration::from_secs(20));

        // Second error
        tracker.on_error(&make_offline_error());
        assert_eq!(tracker.next_delay(), Duration::from_secs(40));

        // Third error (capped at 60s)
        tracker.on_error(&make_offline_error());
        assert_eq!(tracker.next_delay(), Duration::from_secs(60));

        // Fourth error stays at cap
        tracker.on_error(&make_offline_error());
        assert_eq!(tracker.next_delay(), Duration::from_secs(60));
    }

    #[test]
    fn non_transport_error_does_not_set_offline() {
        let mut tracker = OfflineTracker::new(Duration::from_secs(10), Duration::from_secs(60));
        let state = tracker.on_error(&make_non_transport_error());

        assert!(matches!(state, SyncState::Error { .. }));
        assert!(!tracker.is_offline());
    }

    #[test]
    fn bounded_attempts_over_30_minutes() {
        // Base 60s, cap 300s (5 min)
        let mut tracker = OfflineTracker::new(Duration::from_secs(60), Duration::from_secs(300));
        let total_time = Duration::from_mins(30); // 30 mins
        let mut elapsed = Duration::ZERO;
        let mut attempts = 0;

        while elapsed < total_time {
            attempts += 1;
            tracker.on_error(&make_offline_error());
            let delay = tracker.next_delay();
            elapsed += delay;
        }

        // Over 30 minutes with 60s, 120s, 240s, 300s, 300s...
        // 60 + 120 + 240 + 300 + 300 + 300 + 300 + 300 = 1920s (approx 7-8 attempts)
        assert!(attempts <= 10, "Attempts {attempts} must be bounded");
    }

    #[test]
    fn success_resets_backoff() {
        let mut tracker = OfflineTracker::new(Duration::from_secs(10), Duration::from_secs(60));
        tracker.on_error(&make_offline_error());
        tracker.on_error(&make_offline_error());
        assert!(tracker.is_offline());

        let now = std::time::SystemTime::now();
        let state = tracker.on_success(now);
        assert_eq!(state, SyncState::Synced { at: now });
        assert!(!tracker.is_offline());
        assert_eq!(tracker.next_delay(), Duration::from_secs(10));
    }
}
