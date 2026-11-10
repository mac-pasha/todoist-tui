//! FR-18 conflict resolution policy: server state applied first, pending local commands replayed on top (LWW).
//!
//! # FR-18 Conflict Policy
//!
//! Under Todoist Sync API architecture, server-side data represents the authoritative
//! truth for shared resources. However, when a client performs offline mutations or has
//! writes in-flight during a sync cycle, local user intent must not be silently clobbered
//! by stale or concurrent remote reads.
//!
//! To achieve Last-Write-Wins (LWW) with field-level merging (REQ-001 D2):
//! 1. The remote sync response delta is applied atomically to the local cache.
//! 2. Any user command still in [`crate::queue::CommandState::Pending`] that touches an
//!    item present in the sync response is re-applied optimistically to the cache.
//! 3. Cross-field edits merge cleanly: if remote modified `due` and local modified `content`,
//!    re-applying local `content` leaves the new remote `due` intact.

#![allow(dead_code)]

use std::collections::HashSet;

use crate::api::model::{Id, Item};
use crate::api::sync::SyncResponse;
use crate::error::Result;
use crate::queue::QueuedCommand;
use crate::sync::ports::{Cache, WriteQueue};

/// Inspects `pending_commands` and re-applies any pending mutations for items
/// touched in `response`.
///
/// Only commands targeting items that were included in `response.items` (or items
/// modified by the response) trigger cache writes. Commands targeting untouched items
/// do not trigger redundant writes.
pub async fn replay_pending_conflicts<C, Q>(
    cache: &C,
    queue: &Q,
    response: &SyncResponse,
) -> Result<usize>
where
    C: Cache + ?Sized,
    Q: WriteQueue + ?Sized,
{
    if response.items.is_empty() {
        return Ok(0);
    }

    // Set of item IDs present in the incoming sync response
    let touched_ids: HashSet<Id> = response.items.iter().map(|it| it.id.clone()).collect();

    let pending = queue.pending_commands().await?;
    if pending.is_empty() {
        return Ok(0);
    }

    let mut replayed = 0;

    for cmd in pending {
        if let Some(item_id) = extract_item_id(&cmd) {
            if touched_ids.contains(&item_id) {
                replay_item_command(cache, &item_id, &cmd).await?;
                replayed += 1;
            }
        }
    }

    Ok(replayed)
}

fn extract_item_id(cmd: &QueuedCommand) -> Option<Id> {
    let args = &cmd.args;
    match cmd.kind.as_str() {
        "item_update" | "item_close" | "item_complete" | "item_uncomplete" | "item_delete" => args
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(|s| Id::from(s.to_string())),
        _ => None,
    }
}

async fn replay_item_command<C: Cache + ?Sized>(
    cache: &C,
    item_id: &Id,
    cmd: &QueuedCommand,
) -> Result<()> {
    match cmd.kind.as_str() {
        "item_delete" => {
            cache.delete_item(item_id).await?;
        }
        "item_close" | "item_complete" => {
            if let Some(mut item) = cache.item_by_id(item_id).await? {
                item.checked = true;
                cache.upsert_item(&item).await?;
            }
        }
        "item_uncomplete" => {
            if let Some(mut item) = cache.item_by_id(item_id).await? {
                item.checked = false;
                cache.upsert_item(&item).await?;
            }
        }
        "item_update" => {
            if let Some(mut item) = cache.item_by_id(item_id).await? {
                apply_item_update_args(&mut item, &cmd.args);
                cache.upsert_item(&item).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn apply_item_update_args(item: &mut Item, args: &serde_json::Value) {
    if let Some(content) = args.get("content").and_then(serde_json::Value::as_str) {
        item.content = content.to_string();
    }
    if let Some(desc) = args.get("description").and_then(serde_json::Value::as_str) {
        item.description = desc.to_string();
    }
    if let Some(prio) = args.get("priority").and_then(serde_json::Value::as_u64) {
        if let Ok(p) = u8::try_from(prio) {
            if let Ok(priority) = crate::api::model::Priority::new(p) {
                item.priority = priority;
            }
        }
    }
    if let Some(due_val) = args.get("due") {
        if due_val.is_null() {
            item.due = None;
        } else if let Ok(due) = serde_json::from_value(due_val.clone()) {
            item.due = Some(due);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Due, Id, Item};
    use crate::queue::command::QueuedCommand;
    use crate::sync::fakes::fakes_with_log;
    use serde_json::json;

    fn sample_item(id: &str, content: &str) -> Item {
        let v = json!({
            "id": id,
            "user_id": "u1",
            "project_id": "p1",
            "content": content,
            "description": "",
            "child_order": 1,
            "priority": 1,
            "added_at": "2026-01-01T00:00:00Z"
        });
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    async fn remote_content_overwritten_by_pending_local_update() {
        let (_api, cache, queue, _log) = fakes_with_log();

        let item_id = Id::from("item_x");
        let remote_item = sample_item("item_x", "remote");

        // Seed cache with remote item (as if applied from response)
        cache.upsert_item(&remote_item).await.unwrap();

        // Enqueue pending local command
        let cmd = QueuedCommand::new(
            "item_update",
            json!({"id": "item_x", "content": "local"}),
            None,
        );
        queue.enqueue(cmd);

        let mut res = SyncResponse::default();
        res.items.push(remote_item);

        let replayed = replay_pending_conflicts(&cache, &queue, &res)
            .await
            .unwrap();
        assert_eq!(replayed, 1);

        let final_item = cache.item_by_id(&item_id).await.unwrap().unwrap();
        assert_eq!(final_item.content, "local");
    }

    #[tokio::test]
    async fn remote_only_change_retains_remote_value() {
        let (_api, cache, queue, _log) = fakes_with_log();

        let item_id = Id::from("item_y");
        let remote_item = sample_item("item_y", "remote_only");
        cache.upsert_item(&remote_item).await.unwrap();

        let mut res = SyncResponse::default();
        res.items.push(remote_item);

        let replayed = replay_pending_conflicts(&cache, &queue, &res)
            .await
            .unwrap();
        assert_eq!(replayed, 0);

        let final_item = cache.item_by_id(&item_id).await.unwrap().unwrap();
        assert_eq!(final_item.content, "remote_only");
    }

    #[tokio::test]
    async fn untouched_resource_triggers_no_replay() {
        let (_api, cache, queue, _log) = fakes_with_log();

        let pending_cmd = QueuedCommand::new(
            "item_update",
            json!({"id": "item_untouched", "content": "local"}),
            None,
        );
        queue.enqueue(pending_cmd);

        // Response contains item_other, NOT item_untouched
        let other_item = sample_item("item_other", "other");
        let mut res = SyncResponse::default();
        res.items.push(other_item);

        let replayed = replay_pending_conflicts(&cache, &queue, &res)
            .await
            .unwrap();
        assert_eq!(replayed, 0);
    }

    #[tokio::test]
    async fn req_001_d2_cross_field_conflict_preserves_both_edits() {
        let (_api, cache, queue, _log) = fakes_with_log();

        let item_id = Id::from("item_cross");
        let mut remote_item = sample_item("item_cross", "remote_content");
        let remote_due = Due {
            date: "2026-12-25".to_string(),
            string: Some("25 Dec".to_string()),
            lang: None,
            is_recurring: false,
            timezone: None,
        };
        remote_item.due = Some(remote_due.clone());

        // Cache has the applied remote item (with remote due date)
        cache.upsert_item(&remote_item).await.unwrap();

        // Local pending command modified `content` only (not due)
        let local_cmd = QueuedCommand::new(
            "item_update",
            json!({"id": "item_cross", "content": "local_new_content"}),
            None,
        );
        queue.enqueue(local_cmd);

        let mut res = SyncResponse::default();
        res.items.push(remote_item);

        let replayed = replay_pending_conflicts(&cache, &queue, &res)
            .await
            .unwrap();
        assert_eq!(replayed, 1);

        let merged = cache.item_by_id(&item_id).await.unwrap().unwrap();
        // Local content survived
        assert_eq!(merged.content, "local_new_content");
        // Remote due survived!
        assert_eq!(merged.due, Some(remote_due));
    }
}
