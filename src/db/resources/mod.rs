//! Row (de)serialisation for Sync API resources (T-305/T-306, EPIC-04).
//!
//! Each submodule owns upsert/delete (or, for `day_orders`, a wholesale
//! `replace_all`) for one Sync API resource table from `schema.sql`,
//! mapping the domain structs in [`crate::api::model`] onto the typed
//! columns those tables expose for the read-path queries, while the full
//! JSON payload always rides along in `raw_json` so no field is lost to a
//! column a task didn't model.
//!
//! `note`'s `item_id`/`project_id` parent is recorded in `schema.sql`'s
//! `notes.item_id` column (`NOT NULL`); see [`note`]'s module doc for how
//! project-level notes (which carry a `project_id` instead of an
//! `item_id`) are represented there without a schema change.
//!
//! `user` is the one single-row resource here: it is keyed by the
//! authenticated account's own id, and [`user::upsert`] removes any other
//! row before writing so the table never holds more than one account's
//! data at a time (see [`user`]'s module doc).

// `pub` alone does not silence `dead_code` in a binary crate — same
// pattern as `db::mod` and the other EPIC-04 module scaffolds.
// TODO(T-307): remove this allow once T-307 wires these resources into
// the sync-apply path and they start getting called from non-test code.
#![allow(dead_code)]

pub mod day_order;
pub mod filter;
pub mod item;
pub mod label;
pub mod note;
pub mod project;
pub mod section;
pub mod user;
