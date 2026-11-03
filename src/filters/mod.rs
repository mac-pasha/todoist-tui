//! Filter query parsing for the supported grammar subset (T-903, EPIC-10).
//!
//! `parse::parse` turns a saved filter's raw `query` string (as stored on
//! `Filter` in `src/api/model/filter.rs`) into a [`parse::FilterAst`] or a
//! [`parse::Unsupported`] verdict, per the grammar fixed in
//! `docs/manual/epic-10-filter-support.md`. This module only parses: there
//! is no evaluator here (that is T-905) and no cache access (EPIC-04 has
//! not landed). The fallback rule from the spike — any unsupported
//! construct anywhere in the query rejects the whole query, never a
//! partial AST — is enforced entirely by `parse::parse` returning a
//! `Result` rather than a best-effort tree.
//!
//! No consumers yet: the sidebar rendering task (T-909) and the evaluator
//! (T-905) call into this module later. `pub` alone does not silence
//! `dead_code` in a binary crate, so allow it at module scope — same
//! pattern as `src/search/mod.rs` (T-902) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod parse;

pub use parse::{parse, FilterAst, Unsupported};
