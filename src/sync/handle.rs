//! Non-blocking caller-side handle (`SyncHandle`) and engine-side driver
//! (`SyncDriver`) for triggering sync cycles, subscribing to `SyncState`
//! transitions, and shutting the sync engine down cleanly (`T-504`).
//!
//! # Trigger coalescing
//!
//! The trigger path acts as a capacity-1 coalescing flag (`AtomicBool` +
//! [`tokio::sync::Notify`]): sending `N` triggers before the engine drains the
//! pending flag collapses them into a single pending trigger so rapid user
//! actions do not queue `N` redundant back-to-back sync cycles.
//!
//! # State subscriptions
//!
//! Callers subscribe via [`SyncHandle::subscribe`], which returns an unbounded
//! [`SyncStateReceiver`] (`tokio::sync::mpsc::UnboundedReceiver<SyncState>`).
//! When the engine emits a state update via [`SyncDriver::emit`], closed/dropped
//! subscriber receivers are pruned automatically so slow or dropped subscribers
//! never block or fail the engine.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, Notify};

use crate::error::{Result, SyncError};
use crate::sync::state::{SyncState, SyncStatus};

/// Receiver half of a [`SyncHandle::subscribe`] stream of [`SyncState`] updates.
pub type SyncStateReceiver = mpsc::UnboundedReceiver<SyncState>;

/// Alias matching the task specification (`Receiver<SyncState>`).
pub type Receiver<T = SyncState> = mpsc::UnboundedReceiver<T>;

#[derive(Debug)]
struct SharedState {
    /// Capacity-1 coalescing trigger flag (`true` when at least one trigger has
    /// been requested since the engine last consumed one).
    trigger_pending: AtomicBool,
    /// Set to `true` once [`SyncHandle::shutdown`] is invoked or the engine-side
    /// [`SyncDriver`] is dropped.
    closed: AtomicBool,
    /// Set to `true` when the engine loop terminates (`SyncDriver` dropped or
    /// [`SyncDriver::mark_stopped`] called).
    engine_stopped: AtomicBool,
    /// Wakes the engine when a trigger is requested or shutdown occurs.
    trigger_notify: Notify,
    /// Wakes the engine when shutdown is requested.
    shutdown_notify: Notify,
    /// Wakes callers waiting for the engine task to finish.
    done_notify: Notify,
    /// Active subscriber channels receiving [`SyncState`] transitions.
    subscribers: Mutex<Vec<mpsc::UnboundedSender<SyncState>>>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            trigger_pending: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            engine_stopped: AtomicBool::new(false),
            trigger_notify: Notify::new(),
            shutdown_notify: Notify::new(),
            done_notify: Notify::new(),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    fn lock_subscribers(&self) -> std::sync::MutexGuard<'_, Vec<mpsc::UnboundedSender<SyncState>>> {
        match self.subscribers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Cheaply-cloneable handle held by UI/CLI callers to interact with the
/// background sync engine (`T-504`).
#[derive(Clone, Debug)]
pub struct SyncHandle {
    inner: Arc<SharedState>,
}

impl SyncHandle {
    /// Construct a linked `(SyncHandle, SyncDriver)` pair.
    #[must_use]
    pub fn channel() -> (Self, SyncDriver) {
        let inner = Arc::new(SharedState::new());
        let handle = Self {
            inner: Arc::clone(&inner),
        };
        let driver = SyncDriver {
            inner,
            status: SyncStatus::default(),
        };
        (handle, driver)
    }

    /// Alias for [`Self::channel`].
    #[must_use]
    pub fn new() -> (Self, SyncDriver) {
        Self::channel()
    }

    /// Request an immediate sync cycle without blocking.
    ///
    /// Multiple calls before the engine drains the flag collapse into a single
    /// pending trigger.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::ChannelClosed`] wrapped in [`crate::error::AppError::Sync`]
    /// if the engine has been shut down or dropped.
    pub fn trigger(&self) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SyncError::ChannelClosed.into());
        }
        self.inner.trigger_pending.store(true, Ordering::Release);
        // Re-check after storing in case shutdown raced with our store.
        if self.inner.closed.load(Ordering::Acquire) {
            self.inner.trigger_pending.store(false, Ordering::Release);
            return Err(SyncError::ChannelClosed.into());
        }
        self.inner.trigger_notify.notify_one();
        Ok(())
    }

    /// Subscribe to [`SyncState`] transitions emitted by the engine.
    ///
    /// Dropping the returned receiver is safe; the engine prunes disconnected
    /// subscribers on the next state broadcast.
    #[must_use]
    pub fn subscribe(&self) -> SyncStateReceiver {
        let (tx, rx) = mpsc::unbounded_channel();
        if !self.inner.closed.load(Ordering::Acquire) {
            let mut subs = self.inner.lock_subscribers();
            if !self.inner.closed.load(Ordering::Acquire) {
                subs.push(tx);
            }
        }
        rx
    }

    /// Signal the engine loop to stop and close the trigger channel.
    ///
    /// Calling `shutdown` more than once is a no-op (idempotent).
    pub fn shutdown(&self) {
        let was_closed = self.inner.closed.swap(true, Ordering::AcqRel);
        self.inner.trigger_pending.store(false, Ordering::Release);
        if !was_closed {
            self.inner.shutdown_notify.notify_waiters();
            self.inner.trigger_notify.notify_waiters();
        }
    }

    /// Returns `true` if [`Self::shutdown`] has been called or the engine has
    /// exited.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Returns `true` if a coalesced trigger is currently pending.
    #[must_use]
    pub fn has_pending_trigger(&self) -> bool {
        self.inner.trigger_pending.load(Ordering::Acquire)
    }

    /// Returns the number of pending triggers in the coalescing slot (`0` or `1`).
    #[must_use]
    pub fn pending_triggers(&self) -> usize {
        usize::from(self.has_pending_trigger())
    }

    /// Wait until the engine driver has stopped or been dropped.
    pub async fn stopped(&self) {
        loop {
            let notified = self.inner.done_notify.notified();
            if self.inner.engine_stopped.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// Engine-side endpoint paired with [`SyncHandle`].
///
/// Consumes coalesced trigger notifications, broadcasts deduplicated
/// [`SyncState`] transitions via [`SyncStatus`], and observes [`SyncHandle::shutdown`].
#[derive(Debug)]
pub struct SyncDriver {
    inner: Arc<SharedState>,
    status: SyncStatus,
}

impl SyncDriver {
    /// Atomically consume a pending trigger if one is set.
    ///
    /// Returns `true` if a pending trigger was consumed, or `false` if none was
    /// pending (or the channel is shut down).
    #[must_use]
    pub fn try_take_trigger(&self) -> bool {
        if self.inner.closed.load(Ordering::Acquire) {
            self.inner.trigger_pending.store(false, Ordering::Release);
            return false;
        }
        self.inner.trigger_pending.swap(false, Ordering::AcqRel)
    }

    /// Returns the number of pending triggers (`0` or `1`).
    #[must_use]
    pub fn pending_triggers(&self) -> usize {
        usize::from(self.inner.trigger_pending.load(Ordering::Acquire))
    }

    /// Wait until either a trigger is available (consuming it and returning
    /// `Some(())`) or shutdown is requested (returning `None`).
    pub async fn wait_for_trigger(&self) -> Option<()> {
        loop {
            let notified = self.inner.trigger_notify.notified();
            if self.inner.closed.load(Ordering::Acquire) {
                return None;
            }
            if self.try_take_trigger() {
                return Some(());
            }
            notified.await;
        }
    }

    /// Wait until [`SyncHandle::shutdown`] is invoked.
    pub async fn wait_for_shutdown(&self) {
        loop {
            let notified = self.inner.shutdown_notify.notified();
            if self.inner.closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Returns `true` if [`SyncHandle::shutdown`] has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Apply `state` to the internal [`SyncStatus`] tracker and broadcast it to
    /// all live subscribers if it represents a state change.
    ///
    /// Returns `true` when the state changed and was broadcast, or `false` if
    /// it was suppressed as a duplicate of the current state.
    #[allow(clippy::needless_pass_by_value)]
    pub fn emit(&mut self, state: SyncState) -> bool {
        if !self.status.apply(state.clone()) {
            return false;
        }
        self.emit_raw(&state);
        true
    }

    /// Broadcast `state` to all live subscribers unconditionally (without
    /// checking [`SyncStatus`] deduplication).
    ///
    /// Disconnected subscribers are removed in place so slow or dropped
    /// receivers never cause an error or block the caller.
    pub fn emit_raw(&self, state: &SyncState) {
        let mut subs = self.inner.lock_subscribers();
        subs.retain(|tx| tx.send(state.clone()).is_ok());
    }

    /// Borrow the engine's current [`SyncStatus`] tracker.
    #[must_use]
    pub const fn status(&self) -> &SyncStatus {
        &self.status
    }

    /// Mutably borrow the engine's [`SyncStatus`] tracker.
    pub fn status_mut(&mut self) -> &mut SyncStatus {
        &mut self.status
    }

    /// Mark the engine as stopped and close all subscriber channels.
    pub fn mark_stopped(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.trigger_pending.store(false, Ordering::Release);
        self.inner.engine_stopped.store(true, Ordering::Release);
        self.inner.lock_subscribers().clear();
        self.inner.shutdown_notify.notify_waiters();
        self.inner.trigger_notify.notify_waiters();
        self.inner.done_notify.notify_waiters();
    }
}

impl Drop for SyncDriver {
    fn drop(&mut self) {
        self.mark_stopped();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::error::AppError;

    fn assert_send_sync<T: Send + Sync + Clone>() {}

    #[test]
    fn sync_handle_is_clone_send_and_sync() {
        assert_send_sync::<SyncHandle>();
    }

    #[test]
    fn ten_triggers_sent_before_reading_collapse_into_one_pending_trigger() {
        let (handle, driver) = SyncHandle::channel();

        for _ in 0..10 {
            handle.trigger().expect("trigger should succeed while open");
        }

        assert_eq!(handle.pending_triggers(), 1);
        assert_eq!(driver.pending_triggers(), 1);

        // Consuming once drains the coalesced trigger.
        assert!(driver.try_take_trigger());
        assert_eq!(driver.pending_triggers(), 0);
        assert!(!driver.try_take_trigger());
    }

    #[test]
    fn trigger_on_shut_down_engine_returns_channel_closed_error() {
        let (handle, _driver) = SyncHandle::channel();
        handle.shutdown();

        let err = handle
            .trigger()
            .expect_err("trigger after shutdown must error");
        assert!(matches!(err, AppError::Sync(SyncError::ChannelClosed)));
    }

    #[test]
    fn trigger_after_driver_dropped_returns_channel_closed_error() {
        let (handle, driver) = SyncHandle::channel();
        drop(driver);

        let err = handle
            .trigger()
            .expect_err("trigger after driver drop must error");
        assert!(matches!(err, AppError::Sync(SyncError::ChannelClosed)));
    }

    #[test]
    fn dropping_subscriber_receiver_then_emitting_does_not_block_or_error() {
        let (handle, mut driver) = SyncHandle::channel();
        let rx_dropped = handle.subscribe();
        let mut rx_alive = handle.subscribe();

        drop(rx_dropped);

        // Emitting with one dropped subscriber and one live subscriber must
        // succeed without blocking and prune the dropped subscriber.
        assert!(driver.emit(SyncState::Syncing));
        assert_eq!(rx_alive.try_recv().unwrap(), SyncState::Syncing);

        // Drop the remaining subscriber too and emit again.
        drop(rx_alive);
        assert!(driver.emit(SyncState::Offline));
    }

    #[test]
    fn duplicate_state_transitions_are_suppressed_by_emit() {
        let (handle, mut driver) = SyncHandle::channel();
        let mut rx = handle.subscribe();

        // Initial SyncStatus state is Offline, so emitting Offline is a no-op.
        assert!(!driver.emit(SyncState::Offline));
        assert!(rx.try_recv().is_err());

        // Transitioning Offline -> Syncing -> Syncing only emits once.
        assert!(driver.emit(SyncState::Syncing));
        assert!(!driver.emit(SyncState::Syncing));
        assert_eq!(rx.try_recv().unwrap(), SyncState::Syncing);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn calling_shutdown_twice_is_idempotent() {
        let (handle, driver) = SyncHandle::channel();
        assert!(!handle.is_shutdown());
        assert!(!driver.is_shutdown());

        handle.shutdown();
        assert!(handle.is_shutdown());
        assert!(driver.is_shutdown());

        // Second call must be a clean no-op.
        handle.shutdown();
        assert!(handle.is_shutdown());
        assert!(driver.is_shutdown());
    }

    #[tokio::test]
    async fn wait_for_trigger_wakes_on_trigger_and_returns_none_on_shutdown() {
        let (handle, driver) = SyncHandle::channel();

        handle.trigger().unwrap();
        assert_eq!(driver.wait_for_trigger().await, Some(()));

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            handle_clone.shutdown();
        });

        assert_eq!(driver.wait_for_trigger().await, None);
    }
}
