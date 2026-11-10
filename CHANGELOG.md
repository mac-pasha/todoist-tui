# Changelog

All notable changes to `todoist-tui` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Release extraction contract (`vX.Y.Z`):** Top-level version headings follow exact
> `## [X.Y.Z] - YYYY-MM-DD` Keep-a-Changelog formatting so tag-based release automation
> (`v*.*.*` in `.github/workflows/release.yml`, see `docs/manual/epic-12-release-runbook.md`)
> can deterministically match and extract the notes for version `X.Y.Z`.

## [Unreleased]

## [0.1.0] - 2026-11-10

### Added
- **CLI & Config (`EPIC-01`)**: `clap` subcommands (`auth`, `logout`, `sync`, `add`, `today`, `tui`), strict TOML configuration (`sync_interval`, `theme`, `keymap`, `default_view`), XDG/platform directory resolution with `0700` permissions, and daily rotating tracing logs with `Secret` (`***`) redaction.
- **Auth & Token Hygiene (`EPIC-02`)**: OS keychain primary storage with `0600` file fallback (`tighten-not-reject` on upgrade), non-echo TTY/piped token input validation, idempotent `logout` clearing both token stores and local SQLite cache.
- **Sync API Client (`EPIC-03`)**: `rustls`-only HTTPS Todoist Sync API v1 client (`HTTPS` enforced outside loopback test harnesses), exponential backoff with `Retry-After` handling on `429`, immediate fail-fast on `401`/`403`, and typed data models.
- **Local Cache (`EPIC-04`)**: Embedded bundled SQLite store (`PRAGMA user_version` migrations, `0600` file perms), transactional full/incremental delta application, and indexed view queries (`inbox`, `today`, `upcoming`, `project`, `label`).
- **Offline Write Queue (`EPIC-05`)**: Atomic command enqueueing alongside optimistic cache updates, v4 `uuid` idempotent flush deduplication, `temp_id_mapping` cross-reference rewrites, crash recovery of `in_flight` commands, and failed-command reporting.
- **Background Sync Engine (`EPIC-06`)**: Flush-then-pull sync cycles, exponential offline backoff, full-sync lag follow-up, last-write-wins pending conflict replay, and live status events.
- **Terminal UI Shell (`EPIC-07`)**: Three-pane `ratatui` shell (Sidebar / Task List / Detail + Comments + Status Bar + `?` Help Overlay), panic-safe `TerminalGuard`, configurable vim keymap, and ANSI-16 / ANSI-256 / TrueColor themes.
- **Task Actions (`EPIC-08`)**: Optimistic offline-first add, sub-task add, full edit, quick priority/due edits, complete/uncomplete, delete confirmation, move across projects/sections, manual reorder, comments modal, and sync-errors dismissal modal.
- **Structure Management (`EPIC-09`)**: Optimistic project, sub-project, section, and personal label create/rename/reorder/archive/delete workflows with cascade previews, case-insensitive label dedup, and automatic sidebar selection repair.
- **Search & Saved Filters (`EPIC-10`)**: Preallocated fuzzy `/` search over `content` + `description` (`<16ms` p95 at 5k items in release builds) with scope toggle (`Tab`) and `Esc` snapshot restore, plus read-only saved Filter views rejecting unsupported server-side queries cleanly.
- **Non-Interactive CLI (`EPIC-11`)**: One-shot `todoist-tui today [--json]`, `todoist-tui add <text>` (online quick-add + offline queue fallback), and `todoist-tui sync` commands with documented exit codes (`0`, `1`, `2`, `3`, `4`).
- **Hardening & Packaging (`EPIC-12`)**: Hermetic `MockSyncApi` + `TestHome` test suites, 5k-task performance regression gates, automated trace-log secret/PII scan, documentation drift checks (`tests/docs_sync.rs`), CI integration/perf workflows, and release packaging.

