//! Row (de)serialisation for Sync API resources (T-305, EPIC-04).
//!
//! Each submodule owns `upsert`/`delete` for one Sync API resource table
//! from `schema.sql`, mapping the domain structs in [`crate::api::model`]
//! onto the typed columns those tables expose for the read-path queries,
//! while the full JSON payload always rides along in `raw_json` so no
//! field is lost to a column a task didn't model.
//!
//! `user` is the one single-row resource here: it is keyed by the
//! authenticated account's own id, and [`user::upsert`] removes any other
//! row before writing so the table never holds more than one account's
//! data at a time (see [`user`]'s module doc).

// `pub` alone does not silence `dead_code` in a binary crate — same
// pattern as `db::mod` and the other EPIC-04 module scaffolds. T-307
// wires these resources into the sync-apply path; until then nothing
// outside tests calls them.
#![allow(dead_code)]

pub mod filter;
pub mod label;
pub mod project;
pub mod section;
pub mod user;
