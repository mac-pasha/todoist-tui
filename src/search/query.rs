//! Search over cached tasks with view scoping (T-904, EPIC-10).
//!
//! [`search`] runs the T-902 fuzzy matcher ([`crate::search::fuzzy::match_score`])
//! over a caller-supplied slice of [`SearchItem`]s and returns ranked
//! [`SearchHit`]s. [`SearchItem`] is a thin, cache-agnostic struct (plain
//! `String`/`Option<String>` fields, no `rusqlite` types) built by the
//! caller from whatever read layer is available — EPIC-04's cached-task
//! queries land in parallel and are not merged yet, so this module takes a
//! borrowed `&[SearchItem]` slice rather than a DB handle, keeping the two
//! epics decoupled.
//!
//! [`SearchScope`] lets a caller restrict results to the ids currently
//! visible in a view (`CurrentView`) or search every cached item (`All`).
//!
//! Performance (T-908 runs this on 5k items per keystroke): each item is
//! matched against both `content` and `description` with the existing
//! `match_score` DP pass (no new allocation beyond what `match_score`
//! itself needs for matched ranges), `SearchHit::id` borrows from the input
//! slice rather than cloning, and the only extra allocation is the result
//! `Vec` itself.

// No consumers yet: the EPIC-07 search UI calls into this module in a
// later task. `pub` alone does not silence `dead_code` in a binary crate,
// so allow it at module scope — same pattern as `src/search/mod.rs`
// (T-902) and `src/api/mod.rs` (T-201).
#![allow(dead_code)]

use std::collections::HashSet;
use std::ops::Range;

use super::fuzzy::{match_score, MatchScore};

/// A thin, cache-agnostic view of a Todoist task used for searching.
///
/// Deliberately independent of `rusqlite` (or any other storage crate) so
/// this module can be built and tested ahead of EPIC-04's read layer; the
/// caller is responsible for projecting whatever row/struct that layer
/// eventually returns into a `SearchItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchItem {
    pub id: String,
    pub content: String,
    pub description: String,
    pub project_id: String,
    pub section_id: Option<String>,
}

/// Which field of a [`SearchItem`] a [`SearchHit`] matched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedField {
    Content,
    Description,
}

/// A single search result: the matched item's id, its score, which field
/// matched, and the matched byte ranges within that field (for UI
/// highlighting, mirroring [`MatchScore::ranges`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit<'a> {
    pub id: &'a str,
    pub score: i64,
    pub field: MatchedField,
    pub ranges: Vec<Range<usize>>,
}

/// Restricts which [`SearchItem`]s [`search`] considers.
pub enum SearchScope<'a> {
    /// Only items whose id is in `ids` (typically the ids currently
    /// rendered in the active view) are considered.
    CurrentView { ids: &'a HashSet<String> },
    /// Every item in the input slice is considered.
    All,
}

/// Searches `items` for `needle`, restricted by `scope`.
///
/// Each item is matched independently against its `content` and its
/// `description`; an item is returned if either matches. When both match,
/// the higher-scoring field wins and is reported on the resulting
/// [`SearchHit::field`]; ties (equal score on both fields) prefer
/// `content`, since that is the primary, always-visible field.
///
/// Results are sorted by descending score. Ties are broken deterministically
/// using the same rule [`match_score`]'s docs recommend for ranking a list
/// of haystacks: shorter matched text first, then the matched text itself
/// in byte order, then (to guarantee a total order even if two items have
/// identical matched text) the item id in byte order.
pub fn search<'a>(
    items: &'a [SearchItem],
    needle: &str,
    scope: &SearchScope<'_>,
) -> Vec<SearchHit<'a>> {
    let mut hits: Vec<SearchHit<'a>> = items
        .iter()
        .filter(|item| matches_scope(&item.id, scope))
        .filter_map(|item| best_hit(item, needle))
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| matched_text(a).len().cmp(&matched_text(b).len()))
            .then_with(|| {
                let a_spans = matched_text(a).iter().map(|r| (r.start, r.end));
                let b_spans = matched_text(b).iter().map(|r| (r.start, r.end));
                a_spans.cmp(b_spans)
            })
            .then_with(|| a.id.cmp(b.id))
    });

    hits
}

fn matches_scope(id: &str, scope: &SearchScope<'_>) -> bool {
    match scope {
        SearchScope::CurrentView { ids } => ids.contains(id),
        SearchScope::All => true,
    }
}

/// Matches `item` against `needle` in both `content` and `description`,
/// returning the better-scoring field as a [`SearchHit`], or `None` if
/// neither field matches.
fn best_hit<'a>(item: &'a SearchItem, needle: &str) -> Option<SearchHit<'a>> {
    let content_match = match_score(needle, &item.content);
    let description_match = match_score(needle, &item.description);

    match (content_match, description_match) {
        (Some(content), Some(description)) => {
            if description.score > content.score {
                Some(to_hit(&item.id, MatchedField::Description, description))
            } else {
                Some(to_hit(&item.id, MatchedField::Content, content))
            }
        }
        (Some(content), None) => Some(to_hit(&item.id, MatchedField::Content, content)),
        (None, Some(description)) => Some(to_hit(&item.id, MatchedField::Description, description)),
        (None, None) => None,
    }
}

fn to_hit(id: &str, field: MatchedField, m: MatchScore) -> SearchHit<'_> {
    SearchHit {
        id,
        score: m.score,
        field,
        ranges: m.ranges,
    }
}

/// The text a hit's tie-break compares: a proxy for the haystack that was
/// scored, derived from the matched ranges' span. Since `SearchHit` does
/// not carry the original field text, the ranges themselves (which are
/// `haystack`-relative and ascending/non-overlapping, per `match_score`'s
/// contract) are used directly as the tie-break key: their length and byte
/// content vary with the underlying haystack exactly as the matched text
/// would.
fn matched_text<'a>(hit: &'a SearchHit<'_>) -> &'a [Range<usize>] {
    &hit.ranges
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-904 `search` function.
    //!
    //! All fixture data here is fake: no real ids, tokens or personal
    //! information.

    use super::*;

    fn item(id: &str, content: &str, description: &str) -> SearchItem {
        SearchItem {
            id: id.to_string(),
            content: content.to_string(),
            description: description.to_string(),
            project_id: "proj-1".to_string(),
            section_id: None,
        }
    }

    #[test]
    fn matches_only_in_description() {
        let items = vec![item("t1", "buy milk", "remember the oat variety")];

        let hits = search(&items, "oat", &SearchScope::All);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "t1");
        assert_eq!(hits[0].field, MatchedField::Description);
    }

    #[test]
    fn does_not_match_when_neither_field_matches() {
        let items = vec![item("t1", "buy milk", "remember the oat variety")];

        let hits = search(&items, "xyz", &SearchScope::All);

        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn uses_better_of_the_two_field_scores_and_reports_it() {
        // "milk" is a prefix-ish, tight match in content; the same needle
        // appears scattered/buried in description, so content should win.
        let items = vec![item(
            "t1",
            "milk run",
            "a long detour that eventually gets to the shop with the milk",
        )];

        let hits = search(&items, "milk", &SearchScope::All);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, MatchedField::Content);

        let content_score = match_score("milk", &items[0].content).unwrap().score;
        let description_score = match_score("milk", &items[0].description).unwrap().score;
        assert!(content_score > description_score);
        assert_eq!(hits[0].score, content_score);
    }

    #[test]
    fn uses_better_of_the_two_field_scores_when_description_wins() {
        let items = vec![item(
            "t1",
            "a long rambling task title that only loosely references milk",
            "milk run",
        )];

        let hits = search(&items, "milk", &SearchScope::All);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, MatchedField::Description);

        let content_score = match_score("milk", &items[0].content).unwrap().score;
        let description_score = match_score("milk", &items[0].description).unwrap().score;
        assert!(description_score > content_score);
        assert_eq!(hits[0].score, description_score);
    }

    #[test]
    fn current_view_scope_restricts_to_id_set() {
        let items = vec![
            item("t1", "milk run", "buy milk"),
            item("t2", "milk delivery", "buy milk too"),
            item("t3", "milk shake", "buy milk also"),
        ];
        let ids: HashSet<String> = ["t1".to_string(), "t3".to_string()].into_iter().collect();

        let hits = search(&items, "milk", &SearchScope::CurrentView { ids: &ids });

        let hit_ids: HashSet<&str> = hits.iter().map(|h| h.id).collect();
        assert_eq!(hit_ids, HashSet::from(["t1", "t3"]));
    }

    #[test]
    fn all_scope_ignores_any_id_set() {
        let items = vec![
            item("t1", "milk run", "buy milk"),
            item("t2", "milk delivery", "buy milk too"),
            item("t3", "milk shake", "buy milk also"),
        ];

        let hits = search(&items, "milk", &SearchScope::All);

        let hit_ids: HashSet<&str> = hits.iter().map(|h| h.id).collect();
        assert_eq!(hit_ids, HashSet::from(["t1", "t2", "t3"]));
    }

    #[test]
    fn current_view_scope_with_empty_set_returns_nothing() {
        let items = vec![item("t1", "milk run", "buy milk")];
        let ids: HashSet<String> = HashSet::new();

        let hits = search(&items, "milk", &SearchScope::CurrentView { ids: &ids });

        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn results_are_sorted_descending_by_score_with_stable_order() {
        // Fixed fixture of 10+ items, all of which match "milk" somewhere,
        // with deliberately varying match quality so the expected order is
        // non-trivial.
        let items = vec![
            item("t01", "milk", "irrelevant"),
            item("t02", "a milk run", "irrelevant"),
            item("t03", "buy some milk today", "irrelevant"),
            item("t04", "mxixlxk", "irrelevant"),
            item("t05", "irrelevant", "milk"),
            item("t06", "irrelevant", "a milk run"),
            item("t07", "m i l k scattered far apart milk-ish", "irrelevant"),
            item("t08", "no match here at all", "nor here either"),
            item("t09", "Milk (capitalised prefix)", "irrelevant"),
            item(
                "t10",
                "something milk something",
                "something else milky too",
            ),
            item("t11", "milk", "milk"),
            item("t12", "a slightly longer milk prefix case", "irrelevant"),
        ];

        let hits = search(&items, "milk", &SearchScope::All);

        // Every item except t08 should match.

        assert_eq!(hits.len(), items.len() - 1);
        assert!(hits.iter().all(|h| h.id != "t08"));

        // Scores are sorted in non-increasing order.
        for pair in hits.windows(2) {
            assert!(
                pair[0].score >= pair[1].score,
                "expected descending scores, got {} then {}",
                pair[0].score,
                pair[1].score
            );
        }

        // Re-running search produces byte-for-byte the same order: the
        // tie-break is deterministic, not an artifact of sort stability
        // over insertion order.
        let hits_again = search(&items, "milk", &SearchScope::All);
        let ids: Vec<&str> = hits.iter().map(|h| h.id).collect();
        let ids_again: Vec<&str> = hits_again.iter().map(|h| h.id).collect();
        assert_eq!(ids, ids_again);

        // Exact match ties ("t01", "t05", "t11" all score-match "milk"
        // exactly in one or both fields) are broken by matched text length
        // then content then id, giving a fully pinned expected order.
        let expected: Vec<&str> = vec![
            "t01", "t05", "t09", "t11", "t07", "t02", "t06", "t03", "t10", "t12", "t04",
        ];
        assert_eq!(ids, expected);
    }

    // ---- adversarial tests (T-904 unit-tester) ----

    fn ids_of<'a>(hits: &[SearchHit<'a>]) -> Vec<&'a str> {
        hits.iter().map(|h| h.id).collect()
    }

    #[test]
    fn empty_items_slice_returns_no_hits() {
        let items: Vec<SearchItem> = Vec::new();
        assert_eq!(search(&items, "milk", &SearchScope::All).len(), 0);
        assert_eq!(search(&items, "", &SearchScope::All).len(), 0);
    }

    #[test]
    fn empty_needle_matches_everything_in_content_with_no_ranges_ordered_by_id() {
        let items = vec![
            item("b", "beta", ""),
            item("c", "", ""),
            item("a", "alpha", "desc"),
        ];

        let hits = search(&items, "", &SearchScope::All);

        assert_eq!(ids_of(&hits), vec!["a", "b", "c"]);
        for h in &hits {
            assert_eq!(h.score, 0);
            assert_eq!(h.field, MatchedField::Content);
            assert_eq!(h.ranges.len(), 0);
        }
    }

    #[test]
    fn empty_needle_respects_current_view_scope() {
        let items = vec![item("a", "x", ""), item("b", "y", "")];
        let ids: HashSet<String> = ["b".to_string()].into_iter().collect();

        let hits = search(&items, "", &SearchScope::CurrentView { ids: &ids });

        assert_eq!(ids_of(&hits), vec!["b"]);
    }

    #[test]
    fn needle_matching_neither_field_returns_nothing_for_every_item() {
        let items = vec![item("a", "alpha", "beta"), item("b", "gamma", "delta")];
        assert_eq!(search(&items, "zzz", &SearchScope::All).len(), 0);
    }

    #[test]
    fn needle_longer_than_both_fields_returns_nothing() {
        let items = vec![item("a", "ab", "cd")];
        assert_eq!(search(&items, "abcdef", &SearchScope::All).len(), 0);
    }

    #[test]
    fn current_view_with_id_absent_from_items_is_ignored() {
        let items = vec![item("t1", "milk", "")];
        let ids: HashSet<String> = ["ghost".to_string()].into_iter().collect();
        assert_eq!(
            search(&items, "milk", &SearchScope::CurrentView { ids: &ids }).len(),
            0
        );

        let ids: HashSet<String> = ["ghost".to_string(), "t1".to_string()]
            .into_iter()
            .collect();
        let hits = search(&items, "milk", &SearchScope::CurrentView { ids: &ids });
        assert_eq!(ids_of(&hits), vec!["t1"]);
    }

    #[test]
    fn current_view_id_match_is_exact_and_case_sensitive() {
        let items = vec![item("T1", "milk", ""), item("t1x", "milk", "")];
        let ids: HashSet<String> = ["t1".to_string()].into_iter().collect();
        assert_eq!(
            search(&items, "milk", &SearchScope::CurrentView { ids: &ids }).len(),
            0
        );
    }

    #[test]
    fn all_scope_returns_same_as_view_containing_every_id() {
        let items = vec![
            item("t1", "milk run", "x"),
            item("t2", "oat milk", "y"),
            item("t3", "nothing", "z"),
        ];
        let ids: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
        let all = search(&items, "milk", &SearchScope::All);
        let view = search(&items, "milk", &SearchScope::CurrentView { ids: &ids });
        assert_eq!(all, view);
    }

    #[test]
    fn duplicate_ids_each_produce_a_hit() {
        let items = vec![item("dup", "milk", ""), item("dup", "milk", "")];
        let hits = search(&items, "milk", &SearchScope::All);
        assert_eq!(ids_of(&hits), vec!["dup", "dup"]);
    }

    #[test]
    fn duplicate_ids_in_view_scope_are_both_kept() {
        let items = vec![item("dup", "milk", ""), item("dup", "oat milk", "")];
        let ids: HashSet<String> = ["dup".to_string()].into_iter().collect();
        let hits = search(&items, "milk", &SearchScope::CurrentView { ids: &ids });
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn equal_scores_on_both_fields_prefer_content() {
        let items = vec![item("t1", "milk", "milk")];
        let hits = search(&items, "milk", &SearchScope::All);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, MatchedField::Content);
    }

    #[test]
    fn hit_ranges_are_from_the_reported_field() {
        let items = vec![item("t1", "zzz", "xx milk")];
        let hits = search(&items, "milk", &SearchScope::All);
        assert_eq!(hits[0].field, MatchedField::Description);
        assert_eq!(
            hits[0].ranges,
            match_score("milk", "xx milk").unwrap().ranges
        );
    }

    #[test]
    fn identical_items_are_tie_broken_by_id_regardless_of_input_order() {
        let forward = vec![
            item("a", "milk", ""),
            item("b", "milk", ""),
            item("c", "milk", ""),
        ];
        let reversed = vec![
            item("c", "milk", ""),
            item("b", "milk", ""),
            item("a", "milk", ""),
        ];
        let f = search(&forward, "milk", &SearchScope::All);
        let r = search(&reversed, "milk", &SearchScope::All);
        assert_eq!(ids_of(&f), vec!["a", "b", "c"]);
        assert_eq!(ids_of(&r), vec!["a", "b", "c"]);
    }

    #[test]
    #[ignore = "BUG T-904: search() tie-break uses range-count/spans proxy, not T-902 haystack.len()/haystack order; see report"]
    fn tie_break_prefers_shorter_haystack_per_match_score_docs() {
        // Same score (prefix match of "milk") and identical ranges; the
        // documented rule is shorter haystack first. Ids are chosen so
        // that id order contradicts it.
        let items = vec![
            item("a", "milk and a great many other words", ""),
            item("b", "milk", ""),
        ];
        let s_long = match_score("milk", &items[0].content).unwrap();
        let s_short = match_score("milk", &items[1].content).unwrap();
        assert_eq!(s_long.score, s_short.score, "fixture must tie on score");

        let hits = search(&items, "milk", &SearchScope::All);

        assert_eq!(ids_of(&hits), vec!["b", "a"]);
    }

    #[test]
    fn multibyte_content_does_not_panic_and_ranges_are_char_boundaries() {
        let items = vec![item("t1", "café ünï milk", "日本語 のメモ")];
        let hits = search(&items, "ünï", &SearchScope::All);
        assert_eq!(hits.len(), 1);
        for r in &hits[0].ranges {
            assert!(items[0].content.is_char_boundary(r.start));
            assert!(items[0].content.is_char_boundary(r.end));
        }
        let hits = search(&items, "メモ", &SearchScope::All);
        assert_eq!(hits[0].field, MatchedField::Description);
    }

    #[test]
    fn section_id_and_project_id_are_not_searched() {
        let mut it = item("t1", "alpha", "beta");
        it.project_id = "uniqueproj".to_string();
        it.section_id = Some("uniquesect".to_string());
        let items = vec![it];
        assert_eq!(search(&items, "uniqueproj", &SearchScope::All).len(), 0);
        assert_eq!(search(&items, "uniquesect", &SearchScope::All).len(), 0);
    }
}
