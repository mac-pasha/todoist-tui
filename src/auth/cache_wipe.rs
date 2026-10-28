//! The `CacheWipe` hook (T-108, EPIC-02).
//!
//! `logout` (T-108) must clear both token backends *and* the local task
//! cache, but the cache database itself is EPIC-04's territory (the SQLite
//! schema, its `delete_all()`, migrations, etc. do not exist yet). This
//! module defines only the seam: [`CacheWipe`] is the trait `logout` calls
//! through, and [`NoopCacheWipe`] is the placeholder implementation wired
//! in today — deliberately inert so this task adds no `rusqlite`/schema
//! code of its own.

use crate::error::Result;

/// Hook `logout` calls to wipe the local task cache.
///
/// Kept storage-agnostic on purpose: EPIC-04 owns the concrete
/// implementation (backed by the SQLite cache database), while `logout`
/// only needs to call `wipe` and report the outcome via `describe`.
pub trait CacheWipe {
    /// Wipes the cache. Must be idempotent: calling it when there is
    /// nothing to wipe returns `Ok(())`, matching
    /// [`crate::auth::source::TokenSource::clear`]'s contract.
    fn wipe(&self) -> Result<()>;

    /// Human-readable name for the user-facing `logout` summary (e.g.
    /// "cache"). Never includes any cached data.
    fn describe(&self) -> &'static str;
}

/// Placeholder [`CacheWipe`] wired into `logout` until EPIC-04 lands the
/// real cache database.
///
/// **EPIC-04 will replace this with the real SQLite `delete_all()`.** This
/// implementation currently deletes nothing: `wipe` is a no-op that always
/// returns `Ok(())`, because there is no cache database to clear yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCacheWipe;

impl CacheWipe for NoopCacheWipe {
    fn wipe(&self) -> Result<()> {
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "cache (not yet implemented)"
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-108 design constraints hold; the
    //! unit-tester adds the full suite separately.
    use super::*;

    #[test]
    fn noop_cache_wipe_always_succeeds_and_names_itself() {
        let cache = NoopCacheWipe;
        assert!(cache.wipe().is_ok());
        assert!(cache.wipe().is_ok(), "must be idempotent");
        assert_eq!(cache.describe(), "cache (not yet implemented)");
    }

    #[test]
    fn trait_is_object_safe() {
        let cache: Box<dyn CacheWipe> = Box::new(NoopCacheWipe);
        assert!(cache.wipe().is_ok());
        assert_ne!(cache.describe(), "");
    }
}
