# EPIC-09 — Managing Projects, Sections, and Labels

## Overview

`todoist-tui` supports creating, renaming, archiving, unarchiving, reordering, and deleting Todoist structural entities—projects, sub-projects, sections, and personal labels—with immediate optimistic UI updates and durable offline Sync API write queuing.

## Architecture & Modules

- **Domain commands (`src/structure/command.rs`)**: Validates trimmed non-empty names (`1..=120` chars via `MAX_NAME_LEN`) and builds Sync API v1 commands (`project_add`, `project_update`, `project_archive`, `project_unarchive`, `project_delete`, `section_add`, `section_update`, `section_reorder`, `section_delete`, `label_add`, `label_update`, `label_delete`).
- **Mutation handlers (`src/structure/{project,section,label}.rs`)**: Execute validation, mint client `temp_id`s where appropriate, and enqueue commands atomically alongside SQLite cache updates via `queue::enqueue::enqueue`.
  - Labels reject case-insensitive duplicates with `StructureError::DuplicateLabel` before enqueuing anything.
  - Label renames propagate across cached tasks carrying the previous label name; label deletes strip the deleted label name from cached tasks.
- **Cascade previews & optimistic cleanup (`src/structure/cascade.rs`)**:
  - `preview_project_delete`, `preview_project_archive`, `preview_section_delete`, and `preview_label_delete` compute affected counts read-only before opening confirmation dialogs.
  - Project delete removes the target project, descendant sub-projects, their sections, and their tasks locally (see `docs/manual/epic-09-cascade-semantics.md`).
  - Project archive marks `is_archived = true` while preserving tasks locally.
- **TUI forms, dialogs & dispatch (`src/ui/structure/{form,confirm,sidebar,dispatch}.rs`)**:
  - Context-gated keybindings (`N` new, `M` edit/rename, `X` delete) operate cleanly in Sidebar and project List views while remaining ignored in the Detail pane.
  - After deletions or archives remove the active view, `refresh_after_structure_change` repairs selection cleanly back to the parent project or Inbox.

## Keybindings

| Key | Context | Action |
| --- | --- | --- |
| `N` | Sidebar / Project List | Create project, sub-project, section, or label (`StructureNew`) |
| `M` | Sidebar / Project List | Rename selected project, section, or label (`StructureEdit`) |
| `X` | Sidebar / Project List | Delete selected structure entity with cascade preview (`StructureDelete`) |

## Archive vs. Delete

Archiving a project keeps its tasks and hides the project from active sidebar navigation, whereas deleting a project cascades through descendant sub-projects, sections, and tasks after showing the affected counts in the confirmation modal.

