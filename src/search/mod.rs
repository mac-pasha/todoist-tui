//! Fuzzy matching and search for task/filter search (T-902/T-904, EPIC-10).
//!
//! `fuzzy::match_score` is the pure matching core (T-902); `query::search`
//! (T-904) runs it in bulk over a caller-supplied slice of cache-agnostic
//! `SearchItem`s, with optional scoping to the current view. The UI
//! highlighting that will consume the matched ranges arrives in a later
//! EPIC-07 task. Keeping this module dependency-free (no cache crate, no
//! UI, no new crate) lets it land ahead of EPIC-04's read layer.

// No consumers yet: later EPIC-07 UI code calls into this module. `pub`
// alone does not silence `dead_code` in a binary crate, so allow it at
// module scope — same pattern as `src/api/mod.rs` (T-201) and
// `src/auth/mod.rs` (T-101).
#![allow(dead_code, unused_imports)]

pub mod fuzzy;
pub mod query;

pub use fuzzy::{match_score, MatchScore};
pub use query::{search, MatchedField, SearchHit, SearchItem, SearchScope};
