//! Persistence for the Sync API `day_orders` resource (T-306, EPIC-04).
//!
//! `day_orders` is not a list of independently keyed resources the way
//! items/notes are — the Sync API sends it as a single JSON object mapping
//! project/item ids to their manual ordering position (see
//! [`crate::api::sync::SyncResponse::day_orders`]), and a full or
//! incremental sync always replaces the map wholesale rather than
//! upserting individual keys. [`schema.sql`](../schema.sql)'s `day_orders`
//! table mirrors that: a single logical row (fixed id `"me"`) holding the
//! whole map as `raw_json`, so this module exposes [`replace_all`] instead
//! of a per-key `upsert`/`delete` pair.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};

use crate::error::{DbError, Result};

/// Fixed id of the single `day_orders` row, mirroring `user`'s `"me"`
/// convention for the other single-row resource in `schema.sql`.
const ROW_ID: &str = "me";

/// Replaces the entire `day_orders` table contents with `day_orders` in a
/// single transaction.
///
/// `day_orders` is tolerant `serde_json::Map<String, Value>` (see
/// [`crate::api::sync::SyncResponse::day_orders`]) so that heterogeneous
/// value shapes the Sync API might send (plain integers today, objects in
/// future) are stored and round-tripped without a code change here.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `day_orders` cannot be serialised to
/// JSON, or [`DbError::Sqlite`] if the replace statement batch fails.
pub fn replace_all(conn: &Connection, day_orders: &Map<String, Value>) -> Result<()> {
    let raw_json =
        serde_json::to_string(day_orders).map_err(|source| DbError::Decode { source })?;

    let tx = conn
        .unchecked_transaction()
        .map_err(|source| DbError::Sqlite { source })?;

    tx.execute("DELETE FROM day_orders", [])
        .map_err(|source| DbError::Sqlite { source })?;
    tx.execute(
        "INSERT INTO day_orders (id, raw_json) VALUES (?1, ?2)",
        params![ROW_ID, raw_json],
    )
    .map_err(|source| DbError::Sqlite { source })?;

    tx.commit().map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Fetches the current `day_orders` map, or an empty map if none has been
/// stored yet.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails, or [`DbError::Decode`]
/// if the stored `raw_json` can no longer be deserialised into a
/// `serde_json::Map`.
pub fn fetch(conn: &Connection) -> Result<Map<String, Value>> {
    let raw_json: Option<String> = conn
        .query_row(
            "SELECT raw_json FROM day_orders WHERE id = ?1",
            params![ROW_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    match raw_json {
        Some(raw_json) => {
            serde_json::from_str(&raw_json).map_err(|source| DbError::Decode { source }.into())
        }
        None => Ok(Map::new()),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for T-306 `day_order` persistence.
    //!
    //! All fixture data is fake: no real ids or personal information.

    use rusqlite::Connection;

    use super::*;
    use crate::db::migrations;

    /// An in-memory database with the T-303 schema applied.
    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        migrations::run(&mut conn).expect("run migrations");
        conn
    }

    #[test]
    fn fetch_with_no_rows_returns_empty_map() {
        let conn = test_conn();
        assert_eq!(fetch(&conn).unwrap(), Map::new());
    }

    #[test]
    fn replace_all_then_fetch_roundtrips() {
        let conn = test_conn();
        let mut day_orders = Map::new();
        day_orders.insert("item-1".to_string(), Value::from(1));
        day_orders.insert("item-2".to_string(), Value::from(2));

        replace_all(&conn, &day_orders).unwrap();

        assert_eq!(fetch(&conn).unwrap(), day_orders);
    }

    #[test]
    fn replace_all_is_idempotent_and_wholesale() {
        let conn = test_conn();
        let mut first = Map::new();
        first.insert("item-1".to_string(), Value::from(1));
        replace_all(&conn, &first).unwrap();
        replace_all(&conn, &first).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM day_orders", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // A second `replace_all` with a disjoint map must replace the
        // whole table contents, not merge with the previous map.
        let mut second = Map::new();
        second.insert("item-2".to_string(), Value::from(2));
        replace_all(&conn, &second).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM day_orders", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(fetch(&conn).unwrap(), second);
    }
}
