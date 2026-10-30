# EPIC-09 manual: server-side cascade semantics for project, section and label mutations

This page records what the Todoist Sync API does to the resources *below* a
project, section or label when that resource is deleted, archived or
unarchived, and turns each finding into the exact rule the local optimistic
layer must implement. It is the artefact the EPIC-09 risk item asks for
("server-side cascade semantics for delete … must be verified against the real
API before finalising local optimistic behaviour") and it gates T-806
(`src/structure/cascade.rs`). Everything here is about cache semantics, not UI
copy; the confirmation dialogs that consume the resulting counts belong to
T-807.

Every id on this page is a placeholder of the form `6X7Vfake…`, every name is
invented, and no token, account id, email address or personal content appears
anywhere in this file — the same convention as `tests/fixtures/sync_full.json`
(see hard rule 7 in `CLAUDE.md`).

## 1. Method and evidence grades

Four evidence grades are used below, one per statement:

| Grade | Meaning | Source |
|---|---|---|
| **Documented** | The published Todoist Sync API reference states it for the named command. Cited by section path so it can be re-checked quickly. | Todoist Sync API reference → *Commands* → the named command |
| **Captured** | A token-free, id-anonymised incremental `/sync` response taken from a throwaway test account settled it. No row uses this grade yet — see §5. | §5 procedure |
| **Repo** | Provable inside this repository without network access. Cited by file. | `src/api/model/*.rs`, `tests/fixtures/sync_full.json`, `docs/tasks/EPIC-09.graph.json` |
| **Unverified** | Neither of the above settles it. Gets a conservative fallback rule and is listed again in `## Unverified`. | none |

No live `/sync` capture is committed for this page: this spike was written
without an account-scoped token in the environment, which is the same policy
the EPIC-03 fixture follows (hand-maintained fake shape, never a raw response
copied into the repo — `docs/manual/epic-03-sync-api.md` §4). A **Documented**
row names the reference section that settles the question rather than quoting
it; it was not re-fetched while writing this page, so treat it as a pointer to
re-check once (the §5 pass covers all of them) rather than as a verbatim
quote. Statements the reference does not settle are **not** upgraded by
inference: they go to `## Unverified` with a fallback rule.

The reference cited throughout as *Sync API reference, Commands → `<command>`*
is the Todoist developer documentation's Sync API page, section **Commands**
(the table that lists each command `type` — `project_delete`, `project_archive`,
`project_unarchive`, `section_delete`, `label_delete`, … — with its argument
object), read together with the same page's per-resource sections for
`projects`, `sections`, `items` and `labels`. That is the only external source
this page relies on; every other statement points at a file in this repo.

## 2. Deletion and archiving arrive as flags, not as absence

Both the wire model and the pending work in this epic assume the delta reports
the *fate* of a resource, not just the current set. Repo evidence: `is_deleted`
exists on all four affected types — `Project::is_deleted`, `Section::is_deleted`,
`Label::is_deleted` and `Item::is_deleted` — each read through `flexible_bool`,
so the loader accepts both the JSON boolean and the legacy `0`/`1` encoding
(`src/api/model/{project,section,label,item}.rs`). Archiving is a separate flag
of its own, `is_archived`, on `Project` and `Section` only (plain `bool`, no
legacy encoding). `tests/fixtures/sync_full.json` carries one item with
`"is_deleted": true` for exactly this reason.

Illustrative incremental delta after a `section_delete` — fake ids, shape per
the EPIC-03 manual §1, **not** a captured response. `items` is deliberately
`[]`: whether the real delta carries the affected items (re-homed, with
`section_id: null`) or leaves them out entirely is exactly what `## Unverified`
item 2 asks:

```json
{
  "sync_token": "fakeSyncTokenForT801Example0001",
  "full_sync": false,
  "sections": [
    { "id": "6X7VfakeSec0002", "name": "Sprint", "project_id": "6X7VfakePrj0001",
      "section_order": 2, "is_deleted": true, "is_archived": false }
  ],
  "items": [],
  "projects": [],
  "labels": []
}
```

Two consequences hold regardless of which cascade is real:

- A local optimistic delete does not have to be *undone* by the delta; every
  row the delta carries with `is_deleted: true` is dead, and every row it
  re-sends is current. Divergence is self-healing, so the cost of deleting
  slightly too little is a stale row for one sync cycle, while the cost of
  deleting a row the server still has is a row that reappears (also
  self-healing, but it makes the UI flicker).
- Anything the delta does not mention is unchanged. So a cascade the server
  performs but our cache does not will leave the stale rows in place until the
  delta says otherwise. That is the risk that makes `section_delete` the one
  case where this document chooses to wait (rule 3.4, `## Unverified` item 2):
  the delta *is* expected to carry the answer, because it re-states the
  `items` rows it touched.

## 3. Per-command semantics and the local rule

### 3.1 `project_delete`

**Server behaviour.** Deleting a project deletes the project itself together
with every resource beneath it: its tasks, its sections, and its sub-projects
(with those sub-projects' tasks and sections). The command takes only
`project_id`; there is no "keep children" flag. — **Documented**: Sync API
reference, Commands → `project_delete`; consistent with the product behaviour
the epic description assumes ("deleting a project/section removes or re-homes
its tasks per server semantics").

**Repo.** Sub-projects are modelled as a flat parent pointer, not a tree:
`Project::parent_id: Option<Id>` (`src/api/model/project.rs`). Collecting
descendants therefore means walking `parent_id` transitively, not reading one
row. Items point at a project by `Item::project_id: Id` and at a section by
`Item::section_id: Option<Id>` (`src/api/model/item.rs`), so both are reachable
from the project id alone.

**Local optimistic rule.**

- `on project_delete -> the project row is deleted locally, plus every
  descendant project (transitively by parent_id), plus every section whose
  project_id is in that set, plus every item whose project_id is in that set`

Counts for the confirmation dialog (T-806's summary struct, T-807's dialog) are
the cardinalities of those three sets. Recursive descendant handling is
mandatory for T-806, not optional: leaving a sub-project behind would render a
sidebar branch whose parent no longer exists, and nothing in the delta is
guaranteed to clean it up (see `## Unverified` item 1).

### 3.2 `project_archive`

**Server behaviour.** Archiving is not deletion: the project and all of its
tasks and sections are retained and stay addressable; the project is only
hidden from the usual views, and `is_archived` flips to `true`. — **Documented**:
Sync API reference, Commands → `project_archive`; **Repo**: `is_archived` and
`is_deleted` are independent fields on `Project` and `Section`, and T-802's
acceptance requires `project_archive` and `project_delete` to serialise to
distinct command types, so the two can never collapse into one path.

**Local optimistic rule.**

- `on project_archive -> only the targeted project row's is_archived is set to
  true; no item, section or sub-project row is touched locally`

Items of an archived project stay in the cache and stay linked to it — that is
what lets "unarchive" restore the project with its contents intact, and it
matches the note in the EPIC-09 task graph that `archive` "leaves its items in
the cache" while `delete` delegates to the cascade module. Whether the server
*also* archives the sub-projects is not settled by the reference; per the
fallback policy this document does not guess — see `## Unverified` item 3.

### 3.3 `project_unarchive`

**Server behaviour.** The inverse of 3.2: `is_archived` returns to `false`; the
project and its contents become visible again. Nothing is created or deleted by
the command. — **Documented**: Sync API reference, Commands → `project_unarchive`.

**Local optimistic rule.**

- `on project_unarchive -> only the targeted project row's is_archived is set
  to false; no other row is touched locally`

### 3.4 `section_delete`

**Server behaviour.** The section is removed. What happens to the items that
were in it — deleted, or re-homed to the project with `section_id` cleared — is
the one case this spike could not settle from the reference alone, and it is the
reason the epic called this out. The command carries only `section_id`.
— **Documented** (section removal) / **Unverified** (the item consequence): Sync
API reference, Commands → `section_delete`.

**Repo.** A section is referenced from items by `Item::section_id: Option<Id>`
and from the section side by `Section::project_id: Id`; a section has no back
pointer to items, so any item-level cascade must be computed by scanning items
on `section_id`.

**Local optimistic rule (implementation rule for T-806, fallback form).**

- `on section_delete -> only the section row is deleted locally; items that
  carried that section_id are left untouched and resolved by the next sync
  delta`

Rationale: neither branch of the open question is proven, so the cache does the
one thing that is proven — the section is gone — and writes nothing to the item
rows. Deleting the items locally would be an unrecoverable guess if the server
re-homes them; nulling their `section_id` locally would be an equally unproven
guess at the *other* extreme, and it also discards the id needed to recognise
which incoming delta rows correspond to those items. Waiting means the first
delta after the delete settles it in one step, which is exactly the fallback
policy this document is required to follow.

The cost is that the cache briefly holds items whose `section_id` names a
section row that no longer exists. That is absorbed on the read side, not by
the cascade module: the grouped task list must treat a `section_id` with no
matching cached section as "ungrouped, belongs to the project" rather than
hiding the row or failing. This is a rendering fallback only — no cache write,
nothing that could conflict with the delta. The confirmation dialog must also
not promise "N tasks will be deleted" for a section; it reports the item count
as affected, not destroyed (T-807 wording is that task's call).

### 3.5 `label_delete`

**Server behaviour.** The label object is removed; tasks that carried it are
not deleted, they simply no longer carry it. — **Documented**: Sync API
reference, Commands → `label_delete`, together with the label resource's own
definition; the reference describes labels as a standalone resource that items
reference, with no ownership relation in the other direction.

**Repo.** References from items are by *name*, not id: `Item::labels:
Vec<String>` (`src/api/model/item.rs`, defaulting to an empty `Vec` when the key
is absent). `Label` has only `id`/`name`/`color`/`item_order`/`is_favorite`/
`is_deleted` (`src/api/model/label.rs`) — there is no id-keyed join table, so a
stale name in an item's `labels` array is indistinguishable from a
never-existing label.

**Local optimistic rule.**

- `on label_delete -> the label row is deleted locally, and the label's name is
  removed from every cached item's labels array`

The item-side part is idempotent: if the server also ships rewritten `items`
rows in the same delta, applying them afterwards is a no-op; if it does not,
the local strip is what stops the sidebar and task rows from showing a label
that no longer exists. The same name-based reasoning applies to the rename path
owned by T-805 (`label_update`): it must rewrite cached item label names too,
otherwise every item keeps pointing at the old, now-missing name.

### 3.6 Summary of the rules T-806 must implement

| Command | Local optimistic rule |
|---|---|
| `project_delete` | `on project_delete -> project row deleted, descendant projects deleted, their sections deleted, their items deleted` |
| `project_archive` | `on project_archive -> only the project row's is_archived set true; no other row touched` |
| `project_unarchive` | `on project_unarchive -> only the project row's is_archived set false; no other row touched` |
| `section_delete` | `on section_delete -> only the section row is deleted; items with that section_id are left for the next sync delta` |
| `label_delete` | `on label_delete -> label row deleted; label name stripped from every cached item` |

T-806 implements one function per row (`on_project_delete`, `on_project_archive`,
`on_section_delete`, `on_label_delete`), each taking the caller's open
transaction. `project_unarchive` needs no cascade function of its own — T-803's
`unarchive` sets the flag itself. T-806's `preview_*`/dry-run entry point
computes the same counts without mutating, which is what makes the counts
trustworthy in the delete-confirmation dialog.

## Unverified

Each item is a case this page could not pin down with the evidence available to
this spike. The fallback is the rule to implement now, and each one is chosen
so that a wrong guess is corrected by the next sync delta rather than becoming
permanent divergence; where waiting for the delta is available at no cost, that
is what the fallback does.

1. **Whether `project_delete` emits tombstones for the children.** The
   reference documents that the children are deleted server-side, but not
   whether the following incremental delta contains `is_deleted: true` rows for
   the items/sections/sub-projects, or simply omits them.
   *Fallback:* delete children locally (rule 3.1). This one is not left to the
   delta even though that is the general policy, for two reasons: T-806's
   acceptance explicitly requires recursive local deletion, and a project
   delete the user has just confirmed cannot leave its tasks on screen or a
   sub-project branch whose parent row is gone. The only thing the open
   question changes is whether the next delta is a no-op or repeats the
   tombstones — both outcomes agree with the local state.
2. **What happens to items of a deleted section** (deleted vs. re-homed to the
   project). This is the epic's named risk and the single most important open
   item.
   *Fallback:* delete only the section row; touch no item row and wait for the
   next delta (rule 3.4). This is the strictest reading of "prefer waiting over
   guessing a cascade": both known server behaviours are then applied by the
   delta itself. Only a read-side grouping fallback in the renderer is needed
   so a task whose `section_id` has vanished still appears.
3. **Whether `project_archive` also archives sub-projects.** The command takes
   one `project_id`; nothing in the reference says descendants follow.
   *Fallback:* touch only the targeted project row (rule 3.2) and let the next
   delta set `is_archived` on descendants if the server does archive them.
4. **Whether `project_unarchive` restores descendants that were archived
   together with their parent.**
   *Fallback:* touch only the targeted project row (rule 3.3); the delta is
   authoritative.
5. **Whether `label_delete` also ships rewritten `items` rows in the same
   incremental response**, or only the label tombstone.
   *Fallback:* strip the label name from cached items locally (rule 3.5); the
   operation is idempotent so a delta that repeats it changes nothing.
6. **Whether `Section::is_archived` is ever set by the server** — the field is
   modelled defensively in `src/api/model/section.rs` with `#[serde(default)]`,
   and the reference does not describe a section-level archive action.
   *Fallback:* the cascade module never sets `is_archived` locally on a section;
   whatever the delta delivers is stored as server truth.

## 4. Interaction with the write queue

Deleting a project can orphan queue rows: a pending `item_update` for an item
of the deleted project now refers to a resource the server has already removed.
This page deliberately does **not** define a local queue-cascade rule. The
conservative behaviour is to leave those commands in the queue and let the
server reject them, where the EPIC-05 error-class matrix decides keep-vs-drop
(the `docs/manual/epic-05-error-classes.md` artefact of T-401, encoded by
T-408). Dropping queue rows locally would be a second, unverifiable guess
stacked on top of the cascade guess.

What the cascade module must *not* do, in either direction: renumber
`child_order`/`section_order`/`item_order` locally after a delete. Ordering
values are the server's, and the next delta re-states them.

## 5. How to close the unverified items

The procedure mirrors the EPIC-03 fixture-refresh path and needs a throwaway
personal test account — never a real one, and never a token pasted into a
terminal that is being recorded:

1. On the test account, create a fake structure: one parent project, one child
   project under it, one section in each, two items in the section, one label
   attached to both items. Note the ids.
2. Take a full-sync capture (`sync_token: "*"`) and keep it outside the repo.
3. Issue exactly one command through `POST /api/v1/sync` with a single-entry
   `commands` array, then pull one incremental sync with the previous token.
4. Record only the *presence and shape* of the affected arrays — specifically
   whether child items/sections appear with `is_deleted: true`, and whether
   items reappear with a rewritten `section_id`/`labels`.
5. Update this page: move the item out of `## Unverified`, state the finding
   with the grade **Captured**, and note the date. Anonymise every id to the
   `6X7Vfake…` convention before writing; no raw response, token or account id
   is committed.
6. If a finding contradicts a rule in §3, change §3 and the corresponding
   T-806 function in the same PR — the document is the contract the code cites.
7. Repeat for all six items in `## Unverified` in one sitting, since each needs
   the same setup; the section should end up empty.

## 6. Out of scope

- Shared projects, collaborators and workspace admin (EPIC-09 out-of-scope).
- Completing, rescheduling or re-homing individual tasks (EPIC-08).
- Any UI wording for the confirmation dialogs (T-807).
- Full-sync lag detection and delta application mechanics (EPIC-06; see
  T-513 for the sync-engine manual).
