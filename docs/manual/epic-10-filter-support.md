# EPIC-10 manual: filter query support matrix (spike T-901)

This page fixes, before any code is written for EPIC-10, which Todoist
filter-query constructs the TUI will evaluate against its local cache and
which ones it will refuse to evaluate at all. It is the authoritative input
for T-902 (fuzzy matcher — unrelated), T-903 (parser), T-905 (evaluator) and
T-909 (sidebar filter rendering). User-facing README/install content belongs
to EPIC-12, not here.

Saved filters arrive from the Sync API's `filters` resource, modelled by
`Filter` in `src/api/model/filter.rs`. The only field this epic interprets is
`query` (a free-form Todoist filter expression); `name`, `color`,
`item_order`, `is_favorite` and `is_deleted` are rendered or filtered as-is
and have no bearing on parsing or evaluation. The query string is opaque to
the Sync API — the server returns it verbatim — so the client has to decide
unilaterally whether it can honour it.

Policy for this spike is deliberately conservative: **when in doubt, mark a
construct `unsupported`**. The TUI never guesses at a query it does not fully
understand (see §3).

## 1. Construct support matrix

Every construct the client is expected to encounter is listed below with
exactly one verdict: `supported-locally` or `unsupported`. "Cached columns"
refers to the EPIC-04 read-layer projection of a cached task (project id,
section id, parent id, labels array, priority, due date, deadline date,
completed flag, deleted flag, assignee id) — this document does not name
specific file paths, SQL views or Rust types for those yet because EPIC-04
has not landed. The parser (T-903) and evaluator (T-905) tasks will bind
these column names to real identifiers.

| Construct | Verdict | Cached fields needed to evaluate |
|---|---|---|
| `today` | supported-locally | task due date = injected today |
| `overdue` | supported-locally | task due date < injected today, not completed |
| `#project` (project name) | supported-locally | task project id → project name lookup (exact, case-insensitive) |
| `@label` | supported-locally | task labels array contains the given label name |
| `p1`, `p2`, `p3`, `p4` | supported-locally | task priority (with the Sync-API inversion already handled by `common.rs`) |
| `no date` | supported-locally | task due date column is NULL |
| `no labels` | supported-locally | task labels array is empty |
| `&` (AND) | supported-locally | boolean composition — no column |
| `\|` (OR) | supported-locally | boolean composition — no column |
| `!` (NOT) | supported-locally | boolean composition — no column |
| `(` / `)` grouping | supported-locally | boolean composition — no column |
| `assigned to: <name>` | unsupported | — (shared-project assignee semantics; see §3) |
| `assigned by: <name>` | unsupported | — |
| `shared` / `!shared` | unsupported | — |
| `search: <text>` | unsupported | — (free-text search collides with EPIC-10's own `/` fuzzy search; keep them separate) |
| date ranges, incl. `due before:`, `due after:`, `due: Jan 3`, `3 days`, `next week`, `date: ...`, `created: ...` | unsupported | — (Todoist's date mini-language is a full grammar; see §3) |
| `recurring` / `!recurring` | unsupported | — |
| `##project` (project + sub-projects) | unsupported | — (hierarchical scope, see §3) |
| `/section` | unsupported | — |
| `subtask` / `!subtask` | unsupported | — |
| `view all` | unsupported | — (noise token, no local semantics) |
| anything else (unknown token, free text outside a supported leaf) | unsupported | — |

Note: `no date` deliberately covers `no due date` as well; the parser (T-903)
accepts both spellings and normalises them to the same leaf. `no labels`
likewise covers `no label`.

## 2. Supported leaf predicates — exact semantics

The parser from T-903 is only allowed to emit the leaves below. Each one has
one meaning, which the evaluator (T-905) must implement exactly as stated:

- `today` — task has a due date and that date equals the caller-injected
  "today" value. Completed and deleted tasks are excluded by the evaluator's
  default rule (see §3). The injected date is used instead of `Local::now()`
  so tests are deterministic.
- `overdue` — task has a due date strictly before the injected "today" and
  is not completed. (Deleted tasks are always excluded.)
- `#<project-name>` — task's project id resolves to a project whose name
  matches `<project-name>` case-insensitively and exactly. Project
  hierarchies are not traversed (that is why `##project` is unsupported).
- `@<label-name>` — task's labels array contains `<label-name>`
  case-sensitively (Todoist labels are case-sensitive).
- `p1` / `p2` / `p3` / `p4` — task priority equals the given level, using
  the user-facing numbering (`p1` = highest). The Sync-API-to-user
  conversion is already handled upstream.
- `no date` — task's due date column is NULL (no due date, no deadline-only
  distinction at this layer).
- `no labels` — task's labels array is empty.

The parser does not emit any other leaf kind. Any attempt to produce one is
a bug in T-903.

## 3. Fallback rule (whole-query, not per-leaf)

> **A saved filter whose `query` contains at least one `unsupported`
> construct is rendered as `server-defined / unsupported` in the TUI and is
> never evaluated. The client does not return a partial result set.**

Consequences the implementation must honour:

- If `parse(query)` returns `Unsupported`, the sidebar entry still appears
  (the filter is listed by `name`, `color`, `item_order`), but opening it
  shows an explanatory placeholder — not a filtered task list.
- There is no "best effort" mode. `#Work & assigned to: alice` is rejected
  wholesale, not evaluated as `#Work`. This is why the fallback is defined
  on the raw query string, before any AST is built.
- The parser's `Unsupported { reason }` string is the message rendered in
  the placeholder, so it must be short and user-readable (e.g.
  `unsupported: assigned to:`, `unsupported: date range`,
  `unsupported: unknown token`).
- Completed and deleted tasks are excluded from every evaluated result set
  by default, regardless of query. The spike does not add a query-level
  opt-in for them; that is an EPIC-10 follow-up if needed.

The rule is per-query, not per-saved-filter: two different saved filters are
decided independently.

## 4. Grammar subset for T-903

T-903 implements exactly the grammar below. Anything that does not match it
yields `Unsupported`. Operator precedence is fixed as `!` > `&` > `|`;
parentheses override precedence and must be balanced.

```ebnf
query       = or_expr ;

or_expr     = and_expr , { "|" , and_expr } ;
and_expr    = not_expr , { "&" , not_expr } ;
not_expr    = [ "!" ] , atom ;

atom        = "(" , or_expr , ")"
            | leaf ;

leaf        = "today"
            | "overdue"
            | "no" , ws , ( "date" | "due" , ws , "date" )
            | "no" , ws , ( "labels" | "label" )
            | "p" , ( "1" | "2" | "3" | "4" )
            | "#" , name
            | "@" , name ;

name        = name_char , { name_char } ;
name_char   = ? any Unicode scalar except whitespace, "&", "|", "!", "(", ")" ? ;
ws          = ? one or more ASCII space or tab characters ? ;
```

Lexing rules the parser must enforce:

- Tokens are case-insensitive for keywords (`today`, `overdue`, `no date`,
  `p1`..`p4`) and for `#project` names; `@label` names are case-sensitive.
- Whitespace between tokens is skipped. Implicit AND (two atoms with only
  whitespace between them) is **not** accepted — Todoist's UI accepts it,
  but we require an explicit `&` to keep the grammar unambiguous.
- Any `:` in the input (`assigned to:`, `search:`, `due before:`, ...) is a
  hard sentinel for `Unsupported`; the parser does not need to understand
  the sub-language that follows.
- Unknown identifiers (bare words not matching a keyword, no `#`/`@`/`p`
  sigil) are `Unsupported`.
- Unbalanced parentheses are `Unsupported`.

Token kinds the lexer emits: `LParen`, `RParen`, `Amp`, `Pipe`, `Bang`,
`Hash(name)`, `At(name)`, `Priority(1..=4)`, `Today`, `Overdue`, `NoDate`,
`NoLabels`, `Eof`. Anything else is an immediate `Unsupported` with the
offending lexeme in `reason`.

## 5. Example queries — test fixtures for T-903

These exact strings and verdicts are the fixture set T-903's unit tests
must cover verbatim. They are also the examples T-905 reuses for evaluator
tests where the verdict is `supported-locally`.

| # | Query | Verdict | Notes |
|---|---|---|---|
| 1 | `today` | supported-locally | single leaf |
| 2 | `today & p1` | supported-locally | AND of two leaves |
| 3 | `#Work & (today \| overdue)` | supported-locally | precedence + grouping |
| 4 | `@urgent & !p4` | supported-locally | NOT binds tighter than AND |
| 5 | `no date & no labels` | supported-locally | both NULL-ish leaves |
| 6 | `#Home \| #Errands \| #Shopping` | supported-locally | left-associative OR chain |
| 7 | `assigned to: me` | unsupported | `assigned to:` sentinel |
| 8 | `search: dentist` | unsupported | `search:` sentinel |
| 9 | `due before: Jan 3` | unsupported | date range sub-grammar |
| 10 | `#Work & assigned to: alice` | unsupported | whole-query fallback (rule §3) |
| 11 | `(today \| overdue` | unsupported | unbalanced parentheses |
| 12 | `today foo` | unsupported | unknown bare token / no implicit AND |

Examples 1–6 are the "at least 6 concrete `supported-locally` queries"
required by the T-901 acceptance criteria; 7–12 nail down the common
rejection paths so the fallback rule is proven, not implied.

## 6. Out of scope for this spike

- The parser and evaluator themselves (T-903, T-905).
- Any change to `src/api/model/filter.rs` or to the Sync API wire shape —
  the `query` field is consumed as-is.
- `/`-key fuzzy search over task content (T-902, T-904, T-906, T-907); it
  shares the epic but does not share the grammar.
- Writing or editing filters from the TUI — EPIC-10 is read-only for the
  `filters` resource, per the epic brief.
- Persistence of parse results. Each render re-parses the saved filter's
  `query`; parsing is cheap and the fallback rule depends on seeing the
  raw string.

## 7. Fakes only

Every project name, label and query string in this document is fake fixture
data (`#Work`, `#Home`, `@urgent`, `alice`, `dentist`). No real Todoist
account data, assignee name, project name or saved filter query appears
here or in the tests derived from it.

Behaviour source of truth once implemented:
[`src/api/model/filter.rs`](../../src/api/model/filter.rs) (wire shape only),
plus the T-903/T-905 modules listed in
[`docs/tasks/EPIC-10.graph.json`](../tasks/EPIC-10.graph.json).
