//! Shared JSON fixtures and builders for `sync_engine` (`T-511`) and `sync_offline` (`T-512`) integration tests.

#![allow(clippy::needless_pass_by_value)]

use serde_json::{json, Value};

/// Builds a synthetic project JSON object matching Todoist Sync API v1.
pub fn sample_project(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "color": "charcoal",
        "child_order": 1,
        "collapsed": false,
        "is_deleted": false,
        "is_archived": false,
        "is_favorite": false,
        "view_style": "list",
        "inbox_project": false
    })
}

/// Builds a synthetic item JSON object matching Todoist Sync API v1.
pub fn sample_item(id: &str, project_id: &str, content: &str) -> Value {
    json!({
        "id": id,
        "user_id": "user_fake_01",
        "project_id": project_id,
        "content": content,
        "description": "",
        "priority": 1,
        "child_order": 1,
        "checked": false,
        "is_deleted": false,
        "added_at": "2026-11-01T10:00:00Z"
    })
}

/// Builds a full-sync JSON response payload.
pub fn full_sync_response(sync_token: &str, items: Vec<Value>, projects: Vec<Value>) -> Value {
    json!({
        "sync_token": sync_token,
        "full_sync": true,
        "full_sync_date_utc": Value::Null,
        "temp_id_mapping": {},
        "sync_status": {},
        "items": items,
        "projects": projects,
        "sections": [],
        "labels": [],
        "notes": [],
        "filters": []
    })
}

/// Builds an incremental delta JSON response payload.
pub fn incremental_sync_response(
    sync_token: &str,
    items: Vec<Value>,
    sync_status: Value,
    temp_id_mapping: Value,
) -> Value {
    json!({
        "sync_token": sync_token,
        "full_sync": false,
        "temp_id_mapping": temp_id_mapping,
        "sync_status": sync_status,
        "items": items,
        "projects": [],
        "sections": [],
        "labels": [],
        "notes": [],
        "filters": []
    })
}
