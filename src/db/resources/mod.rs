//! Row (de)serialisation for Sync API resources (T-306, EPIC-04).
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

// `pub` alone does not silence `dead_code` in a binary crate — same
// pattern as `db::mod` and the other EPIC-04 module scaffolds.
#![allow(dead_code)]

pub mod day_order;
pub mod item;
pub mod note;
