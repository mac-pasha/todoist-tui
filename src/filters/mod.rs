//! Filter query parsing for the supported grammar subset (T-903, EPIC-10).
//!
//! `parse::parse` turns a saved filter's raw `query` string (as stored on
//! `Filter` in `src/api/model/filter.rs`) into a [`parse::FilterAst`] or a
//! [`parse::Unsupported`] verdict, per the grammar fixed in
//! `docs/manual/epic-10-filter-support.md`. Parsing touches no cache (that
//! is EPIC-04, not merged). The fallback rule from the spike — any
//! unsupported construct anywhere in the query rejects the whole query,
//! never a partial AST — is enforced entirely by `parse::parse` returning a
//! `Result` rather than a best-effort tree.
//!
//! `eval` (T-905) evaluates a parsed AST over caller-supplied,
//! cache-agnostic [`eval::FilterItem`]s with an injected "today" date
//! ([`eval::NaiveDateLike`], no clock read, deterministic tests);
//! [`eval::resolve`] is parse-then-evaluate and returns
//! [`eval::FilterResult::Unsupported`] instead of a partial result when the
//! query does not parse.
//!
//! No consumers yet: the sidebar rendering task (T-909) calls into this
//! module later. `pub` alone does not silence `dead_code` in a binary
//! crate, so allow it at module scope — same pattern as
//! `src/search/mod.rs` (T-902) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod eval;
pub mod parse;

pub use eval::{evaluate, resolve, FilterItem, FilterResult, ItemId, NaiveDateLike};
pub use parse::{parse, FilterAst, Unsupported};
