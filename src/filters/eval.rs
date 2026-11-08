//! Local evaluation of a parsed filter against cached tasks (T-905,
//! EPIC-10).
//!
//! [`evaluate`] walks a [`FilterAst`] (T-903) over a caller-supplied slice
//! of [`FilterItem`]s and returns the ids of the matching items.
//! [`resolve`] is the parse-then-evaluate convenience: if `parse` rejects
//! the query it returns [`FilterResult::Unsupported`] and never a partial
//! result set (spike fallback rule, `docs/manual/epic-10-filter-support.md`
//! §3).
//!
//! [`FilterItem`] is cache-agnostic (plain `String`/`Option<String>`
//! fields, no `rusqlite`), mirroring `SearchItem` from T-904: EPIC-04's
//! read layer is not merged, so the caller materialises items and passes
//! them in.
//!
//! "Today" is injected as a [`NaiveDateLike`]; nothing here reads the
//! clock, so tests are deterministic. The crate has no `chrono`, so
//! `NaiveDateLike` is a minimal validated `YYYY-MM-DD` value.
//!
//! Completed and deleted items are always excluded: the T-903 grammar
//! defines no leaf that asks for them, so there is no way to opt in.
//!
//! ponytail: date comparison is lexicographic on the `YYYY-MM-DD` prefix of
//! the raw `Due.date` string (date-only, floating `...T10:00:00` and UTC
//! `...Z` all share that prefix). Ceiling: no time-zone conversion (a UTC
//! due near midnight compares by its UTC date, not the user's local date)
//! and no non-ISO formats. Upgrade path: add a real date/time crate when
//! zone-aware "today" is required.

use std::cmp::Ordering;

use super::parse::{parse, FilterAst, Leaf};

/// Id of a matching item (owned copy of [`FilterItem::id`]).
pub type ItemId = String;

/// A validated calendar date, stored as ASCII `YYYY-MM-DD`.
///
/// Stand-in for a `chrono::NaiveDate` (not a dependency). `Copy` so it can
/// be injected freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaiveDateLike([u8; 10]);

impl NaiveDateLike {
    /// Builds a date; `None` if the year is outside `0..=9999` or the
    /// month/day is not a real Gregorian calendar day.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Option<Self> {
        if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let max_day = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if !(1..=max_day).contains(&day) {
            return None;
        }
        let y = year.cast_unsigned();
        let digit = |n: u32| b'0' + (n % 10) as u8;
        Some(NaiveDateLike([
            digit(y / 1000),
            digit(y / 100),
            digit(y / 10),
            digit(y),
            b'-',
            digit(u32::from(month) / 10),
            digit(u32::from(month)),
            b'-',
            digit(u32::from(day) / 10),
            digit(u32::from(day)),
        ]))
    }

    /// Orders the date component of a raw API due string (`2024-05-01`,
    /// `2024-05-01T10:00:00`, `2024-05-01T10:00:00Z`) relative to `self`.
    /// `None` if the string does not start with a well-formed
    /// `YYYY-MM-DD` followed by end-of-string or `T`.
    fn cmp_due(&self, due: &str) -> Option<Ordering> {
        let b = due.as_bytes();
        let prefix = b.get(..10)?;
        let shape_ok = prefix.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        });
        if !shape_ok || b.get(10).is_some_and(|c| *c != b'T') {
            return None;
        }
        // ASCII ISO dates order lexicographically == chronologically.
        Some(prefix.cmp(&self.0[..]))
    }
}

/// A thin, cache-agnostic view of a task used for filter evaluation.
///
/// `due` carries the raw `Due.date` string from `src/api/model/common.rs`
/// rather than a whole `Due`: the evaluator only needs the date, and the
/// other `Due` fields (display string, recurrence) would just be noise for
/// callers. `project` is the project *name* (the caller resolves the id to
/// a name; `#project` matches by name per spike §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterItem {
    pub id: String,
    pub project: Option<String>,
    pub labels: Vec<String>,
    /// User-facing priority, `1` (highest) ..= `4`.
    pub priority: u8,
    /// Raw `Due.date`; `None` means no due date.
    pub due: Option<String>,
    pub is_completed: bool,
    pub is_deleted: bool,
}

/// Outcome of [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterResult {
    /// Query parsed; ids of the matching items, in input order.
    Ok(Vec<ItemId>),
    /// Query rejected by the parser; `reason` is `Unsupported::reason`.
    Unsupported { reason: String },
}

/// Parses `query` and evaluates it over `items`. Any parse rejection yields
/// [`FilterResult::Unsupported`], never a partial result set.
pub fn resolve(query: &str, items: &[FilterItem], today: NaiveDateLike) -> FilterResult {
    match parse(query) {
        Ok(ast) => FilterResult::Ok(evaluate(&ast, items, today)),
        Err(e) => FilterResult::Unsupported { reason: e.reason },
    }
}

/// Returns the ids of live (not completed, not deleted) `items` matching
/// `ast`, in input order. `today` is the injected current date.
pub fn evaluate(ast: &FilterAst, items: &[FilterItem], today: NaiveDateLike) -> Vec<ItemId> {
    items
        .iter()
        .filter(|i| !i.is_completed && !i.is_deleted && matches(ast, i, &today))
        .map(|i| i.id.clone())
        .collect()
}

fn matches(ast: &FilterAst, item: &FilterItem, today: &NaiveDateLike) -> bool {
    match ast {
        FilterAst::And(a, b) => matches(a, item, today) && matches(b, item, today),
        FilterAst::Or(a, b) => matches(a, item, today) || matches(b, item, today),
        FilterAst::Not(inner) => !matches(inner, item, today),
        FilterAst::Leaf(leaf) => matches_leaf(leaf, item, today),
    }
}

fn matches_leaf(leaf: &Leaf, item: &FilterItem, today: &NaiveDateLike) -> bool {
    let due_vs_today = || item.due.as_deref().and_then(|d| today.cmp_due(d));
    match leaf {
        Leaf::Today => due_vs_today() == Some(Ordering::Equal),
        Leaf::Overdue => due_vs_today() == Some(Ordering::Less),
        Leaf::Project(name) => item
            .project
            .as_deref()
            .is_some_and(|p| p.to_lowercase() == name.to_lowercase()),
        Leaf::Label(name) => item.labels.iter().any(|l| l == name),
        Leaf::Priority(p) => item.priority == *p,
        Leaf::NoDate => item.due.is_none(),
        Leaf::NoLabels => item.labels.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDateLike {
        NaiveDateLike::from_ymd(2024, 5, 10).unwrap()
    }

    fn item(
        id: &str,
        project: Option<&str>,
        labels: &[&str],
        p: u8,
        due: Option<&str>,
    ) -> FilterItem {
        FilterItem {
            id: id.into(),
            project: project.map(Into::into),
            labels: labels.iter().map(ToString::to_string).collect(),
            priority: p,
            due: due.map(Into::into),
            is_completed: false,
            is_deleted: false,
        }
    }

    /// a: due today (date-only), Work, @urgent, p1
    /// b: overdue (floating datetime), Home, @Urgent @home, p2
    /// c: future (UTC datetime), Work, no labels, p3
    /// d: no date, Inbox, no labels, p4
    /// e: due today, Work, @urgent, p1 - completed
    /// f: overdue, Work, no labels, p1 - deleted
    fn fixture() -> Vec<FilterItem> {
        let mut e = item("e", Some("Work"), &["urgent"], 1, Some("2024-05-10"));
        e.is_completed = true;
        let mut f = item("f", Some("Work"), &[], 1, Some("2024-05-01"));
        f.is_deleted = true;
        vec![
            item("a", Some("Work"), &["urgent"], 1, Some("2024-05-10")),
            item(
                "b",
                Some("Home"),
                &["Urgent", "home"],
                2,
                Some("2024-05-09T10:00:00"),
            ),
            item("c", Some("Work"), &[], 3, Some("2024-05-11T23:00:00Z")),
            item("d", Some("Inbox"), &[], 4, None),
            e,
            f,
        ]
    }

    fn run(q: &str) -> Vec<String> {
        match resolve(q, &fixture(), today()) {
            FilterResult::Ok(ids) => ids,
            other @ FilterResult::Unsupported { .. } => {
                panic!("expected Ok for {q:?}, got {other:?}")
            }
        }
    }

    #[test]
    fn from_ymd_validates() {
        assert!(NaiveDateLike::from_ymd(2024, 2, 29).is_some());
        assert!(NaiveDateLike::from_ymd(2023, 2, 29).is_none());
        assert!(NaiveDateLike::from_ymd(2100, 2, 29).is_none());
        assert!(NaiveDateLike::from_ymd(2024, 13, 1).is_none());
        assert!(NaiveDateLike::from_ymd(2024, 4, 31).is_none());
        assert!(NaiveDateLike::from_ymd(2024, 1, 0).is_none());
        assert!(NaiveDateLike::from_ymd(-1, 1, 1).is_none());
    }

    #[test]
    fn today_leaf() {
        assert_eq!(run("today"), ["a"]); // positive; b (past) and c (future) negative
    }

    #[test]
    fn overdue_leaf() {
        assert_eq!(run("overdue"), ["b"]); // a (today), c (future), d (no date) negative
    }

    #[test]
    fn project_leaf_is_case_insensitive_exact() {
        assert_eq!(run("#work"), ["a", "c"]);
        assert_eq!(run("#Wor"), Vec::<String>::new());
    }

    #[test]
    fn label_leaf_is_case_sensitive() {
        assert_eq!(run("@urgent"), ["a"]);
        assert_eq!(run("@Urgent"), ["b"]);
        assert_eq!(run("@nope"), Vec::<String>::new());
    }

    #[test]
    fn priority_leaf() {
        assert_eq!(run("p1"), ["a"]);
        assert_eq!(run("p4"), ["d"]);
    }

    #[test]
    fn no_date_leaf() {
        assert_eq!(run("no date"), ["d"]);
    }

    #[test]
    fn no_labels_leaf() {
        assert_eq!(run("no labels"), ["c", "d"]);
    }

    #[test]
    fn composite_and_or() {
        assert_eq!(run("#Work & p1"), ["a"]);
        assert_eq!(run("today | overdue"), ["a", "b"]);
    }

    #[test]
    fn composite_not_and_parens() {
        assert_eq!(run("!#Work"), ["b", "d"]);
        assert_eq!(run("(today | overdue) & !@urgent"), ["b"]);
    }

    #[test]
    fn composite_precedence_and_binds_tighter_than_or() {
        // today | (#Home & p4) -> a only; (today | #Home) & p4 would be empty.
        assert_eq!(run("today | #Home & p4"), ["a"]);
        assert_eq!(run("(today | #Home) & p4"), Vec::<String>::new());
    }

    #[test]
    fn completed_and_deleted_always_excluded() {
        // e (completed, today) and f (deleted, overdue) match the leaves but never appear.
        assert_eq!(run("today"), ["a"]);
        assert_eq!(run("overdue"), ["b"]);
        assert_eq!(run("!p4"), ["a", "b", "c"]);
        assert_eq!(run("#Work | !#Work"), ["a", "b", "c", "d"]);
    }

    #[test]
    fn unsupported_query_yields_no_partial_results() {
        match resolve("#Work & assigned to: alice", &fixture(), today()) {
            FilterResult::Unsupported { reason } => assert_ne!(reason, ""),
            other @ FilterResult::Ok(_) => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn malformed_due_matches_neither_today_nor_overdue() {
        let items = [item("x", None, &[], 1, Some("garbage"))];
        let ast = parse("today | overdue").unwrap();
        assert_eq!(evaluate(&ast, &items, today()), Vec::<String>::new());
    }
}

// ---- adversarial tests (T-905 unit-tester) ----
#[cfg(test)]
mod adversarial_tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> NaiveDateLike {
        NaiveDateLike::from_ymd(y, m, day).unwrap()
    }

    fn it(
        id: &str,
        project: Option<&str>,
        labels: &[&str],
        p: u8,
        due: Option<&str>,
    ) -> FilterItem {
        FilterItem {
            id: id.into(),
            project: project.map(Into::into),
            labels: labels.iter().map(ToString::to_string).collect(),
            priority: p,
            due: due.map(Into::into),
            is_completed: false,
            is_deleted: false,
        }
    }

    fn due_item(due: &str) -> FilterItem {
        it("x", None, &[], 1, Some(due))
    }

    fn ids(q: &str, items: &[FilterItem], today: NaiveDateLike) -> Vec<String> {
        match resolve(q, items, today) {
            FilterResult::Ok(v) => v,
            other @ FilterResult::Unsupported { .. } => panic!("{q:?} -> {other:?}"),
        }
    }

    fn hit(q: &str, due: &str, today: NaiveDateLike) -> bool {
        !ids(q, &[due_item(due)], today).is_empty()
    }

    // -- date prefix / UTC boundary --

    #[test]
    fn utc_midnight_boundary_uses_utc_date_prefix() {
        let t = d(2024, 5, 10);
        assert!(hit("today", "2024-05-10T00:00:00Z", t));
        assert!(hit("today", "2024-05-10T23:59:59Z", t));
        assert!(hit("overdue", "2024-05-09T23:59:59Z", t));
        assert!(!hit("today", "2024-05-09T23:59:59Z", t));
        assert!(!hit("overdue", "2024-05-10T00:00:00Z", t));
        assert!(!hit("today | overdue", "2024-05-11T00:00:00Z", t));
    }

    #[test]
    fn date_ordering_crosses_month_and_year_boundaries() {
        assert!(hit("overdue", "2023-12-31", d(2024, 1, 1)));
        assert!(hit("overdue", "2024-02-29", d(2024, 3, 1)));
        assert!(hit("today", "2024-02-29T08:00:00", d(2024, 2, 29)));
        assert!(!hit("overdue", "2024-03-01", d(2024, 2, 29)));
        // zero-padded years compare correctly
        assert!(hit("overdue", "0999-12-31", d(1000, 1, 1)));
        assert!(hit("today", "0000-01-01", d(0, 1, 1)));
        assert!(hit("overdue", "9999-12-30", d(9999, 12, 31)));
    }

    #[test]
    fn date_only_and_trailing_t_forms_accepted() {
        let t = d(2024, 5, 10);
        assert!(hit("today", "2024-05-10", t));
        assert!(hit("today", "2024-05-10T", t));
        assert!(hit("today", "2024-05-10T10:00:00+02:00", t));
    }

    #[test]
    fn malformed_due_is_never_today_or_overdue_but_still_dated() {
        let t = d(2024, 5, 10);
        let bad = [
            "",
            "2024-05-10X",
            "2024-05-10 10:00",
            "2024-05-10Z",
            "2024-05-10t10:00:00", // lowercase t: API never emits it
            "2024-5-10",
            "202405-10",
            "2024/05/10",
            "2024-05-1",
            "2024-05-1T",
            "20240510T100000",
            "-2024-05-10",
            "\u{ff12}\u{ff10}\u{ff12}\u{ff14}-05-10", // fullwidth digits
            "2024-05-10\u{e9}",                       // multibyte right after the prefix
            "\u{e9}",
            "2024-05-0\u{e9}",
            "garbage-in-here",
        ];
        for due in bad {
            assert!(!hit("today", due, t), "today matched {due:?}");
            assert!(!hit("overdue", due, t), "overdue matched {due:?}");
            // a present-but-bad due is not "no date", and is not dropped
            assert!(!hit("no date", due, t), "no date matched {due:?}");
            assert!(hit("!today & !overdue", due, t), "negation lost {due:?}");
        }
    }

    // -- NaiveDateLike --

    #[test]
    fn from_ymd_encoding_is_zero_padded_iso() {
        assert_eq!(d(2024, 5, 10).0, *b"2024-05-10");
        assert_eq!(d(0, 1, 1).0, *b"0000-01-01");
        assert_eq!(d(5, 12, 31).0, *b"0005-12-31");
        assert_eq!(d(9999, 12, 31).0, *b"9999-12-31");
        assert_eq!(d(2024, 5, 10), d(2024, 5, 10));
        assert_ne!(d(2024, 5, 10), d(2024, 10, 5));
    }

    #[test]
    fn from_ymd_exhaustive_matches_independent_calendar() {
        let leap = |y: i32| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        for y in 0..=9999 {
            for m in 0..=13u8 {
                let dim: u8 = match m {
                    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                    4 | 6 | 9 | 11 => 30,
                    2 if leap(y) => 29,
                    2 => 28,
                    _ => 0,
                };
                for day in [0u8, 1, 28, 29, 30, 31, 32, 255] {
                    let got = NaiveDateLike::from_ymd(y, m, day);
                    if (1..=dim).contains(&day) {
                        let want = format!("{y:04}-{m:02}-{day:02}");
                        assert_eq!(got.map(|g| g.0.to_vec()), Some(want.into_bytes()));
                    } else {
                        assert!(got.is_none(), "{y}-{m}-{day} accepted");
                    }
                }
            }
        }
    }

    #[test]
    fn from_ymd_rejects_out_of_range_years() {
        assert!(NaiveDateLike::from_ymd(-1, 1, 1).is_none());
        assert!(NaiveDateLike::from_ymd(10_000, 1, 1).is_none());
        assert!(NaiveDateLike::from_ymd(i32::MIN, 1, 1).is_none());
        assert!(NaiveDateLike::from_ymd(i32::MAX, 1, 1).is_none());
        assert!(NaiveDateLike::from_ymd(0, 2, 29).is_some()); // year 0 divisible by 400
        assert!(NaiveDateLike::from_ymd(1900, 2, 29).is_none());
        assert!(NaiveDateLike::from_ymd(2000, 2, 29).is_some());
    }

    #[test]
    fn today_is_injected_not_read_from_clock() {
        let items = [due_item("2024-05-10")];
        let ast = parse("today").unwrap();
        assert_eq!(evaluate(&ast, &items, d(2024, 5, 10)), ["x"]);
        assert_eq!(evaluate(&ast, &items, d(2024, 5, 10)), ["x"]);
        assert_eq!(evaluate(&ast, &items, d(2024, 5, 11)), Vec::<String>::new());
        let od = parse("overdue").unwrap();
        assert_eq!(evaluate(&od, &items, d(2024, 5, 11)), ["x"]);
    }

    // -- project / label / priority --

    #[test]
    fn project_case_folding_is_unicode_lowercase_equality() {
        let t = d(2024, 5, 10);
        let items = [
            it("ss", Some("GROSS"), &[], 1, None),
            it("eszett", Some("Gro\u{df}"), &[], 1, None),
            it("istanbul_dot", Some("\u{130}stanbul"), &[], 1, None),
            it("sigma", Some("\u{3a3}\u{391}\u{3a3}"), &[], 1, None),
        ];
        // ß does not fold to ss with to_lowercase
        assert_eq!(ids("#gro\u{df}", &items, t), ["eszett"]);
        assert_eq!(ids("#GROSS", &items, t), ["ss"]);
        // dotted capital I is not plain i
        assert_eq!(ids("#istanbul", &items, t), Vec::<String>::new());
        assert_eq!(ids("#\u{130}stanbul", &items, t), ["istanbul_dot"]);
        // final sigma handled consistently on both sides
        assert_eq!(ids("#\u{3c3}\u{3b1}\u{3c2}", &items, t), ["sigma"]);
        assert_eq!(ids("#\u{3a3}\u{391}\u{3a3}", &items, t), ["sigma"]);
    }

    #[test]
    fn project_none_and_empty_never_match_hash_but_match_negation() {
        let t = d(2024, 5, 10);
        let items = [
            it("none", None, &[], 1, None),
            it("empty", Some(""), &[], 1, None),
            it("work", Some("Work"), &[], 1, None),
        ];
        assert_eq!(ids("#Work", &items, t), ["work"]);
        assert_eq!(ids("!#Work", &items, t), ["none", "empty"]);
        assert_eq!(ids("#Wor", &items, t), Vec::<String>::new()); // no prefix match
        assert_eq!(ids("#Work2", &items, t), Vec::<String>::new());
    }

    #[test]
    fn label_matching_is_exact_not_substring_and_any_of() {
        let t = d(2024, 5, 10);
        let items = [
            it("a", None, &["urgent2"], 1, None),
            it("b", None, &["x", "urgent", "urgent"], 1, None),
            it("c", None, &[""], 1, None),
            it("d", None, &[], 1, None),
        ];
        assert_eq!(ids("@urgent", &items, t), ["b"]); // once despite dup label
        assert_eq!(ids("no labels", &items, t), ["d"]); // [""] is still a label
        assert_eq!(ids("!@urgent", &items, t), ["a", "c", "d"]);
    }

    #[test]
    fn item_priority_outside_1_to_4_matches_no_priority_leaf() {
        let t = d(2024, 5, 10);
        let items = [
            it("p0", None, &[], 0, None),
            it("p5", None, &[], 5, None),
            it("p255", None, &[], 255, None),
            it("p1", None, &[], 1, None),
        ];
        for q in ["p1", "p2", "p3", "p4"] {
            let got = ids(q, &items, t);
            assert!(got.iter().all(|i| i == "p1"), "{q}: {got:?}");
        }
        assert_eq!(ids("!p1", &items, t), ["p0", "p5", "p255"]);
        assert_eq!(
            ids("!p1 & !p2 & !p3 & !p4", &items, t),
            ["p0", "p5", "p255"]
        );
    }

    // -- result shape --

    #[test]
    fn empty_items_yield_empty_ok_not_unsupported() {
        let t = d(2024, 5, 10);
        assert_eq!(resolve("today", &[], t), FilterResult::Ok(vec![]));
        assert_eq!(resolve("!today", &[], t), FilterResult::Ok(vec![]));
        assert_eq!(
            evaluate(&parse("no date").unwrap(), &[], t),
            Vec::<String>::new()
        );
    }

    #[test]
    fn duplicates_and_input_order_preserved() {
        let t = d(2024, 5, 10);
        let items = [
            it("z", None, &[], 1, None),
            it("a", None, &[], 1, None),
            it("z", None, &[], 1, None),
            it("m", None, &[], 2, None),
        ];
        assert_eq!(ids("p1", &items, t), ["z", "a", "z"]);
        assert_eq!(ids("p1 | p2", &items, t), ["z", "a", "z", "m"]);
    }

    #[test]
    fn completed_and_deleted_excluded_even_when_both_flags_and_under_negation() {
        let t = d(2024, 5, 10);
        let mut both = it("both", Some("W"), &[], 1, Some("2024-05-10"));
        both.is_completed = true;
        both.is_deleted = true;
        let mut done = it("done", Some("W"), &[], 1, None);
        done.is_completed = true;
        let mut del = it("del", Some("W"), &[], 1, None);
        del.is_deleted = true;
        let live = it("live", Some("W"), &[], 1, None);
        let items = [both, done, del, live];
        for q in [
            "p1",
            "!p2",
            "#W",
            "no date",
            "no labels",
            "!today",
            "p1 | !p1",
        ] {
            let got = ids(q, &items, t);
            assert!(got.iter().all(|i| i == "live"), "{q}: {got:?}");
        }
        assert_eq!(ids("p1", &items, t), ["live"]);
    }

    // -- deep ASTs --

    fn leaf(l: Leaf) -> FilterAst {
        FilterAst::Leaf(l)
    }

    #[test]
    fn deep_not_chain_alternates_parity() {
        let t = d(2024, 5, 10);
        let items = [it("a", None, &[], 1, None), it("b", None, &[], 2, None)];
        let nest =
            |n: usize| (0..n).fold(leaf(Leaf::Priority(1)), |a, _| FilterAst::Not(Box::new(a)));
        assert_eq!(evaluate(&nest(1000), &items, t), ["a"]);
        assert_eq!(evaluate(&nest(1001), &items, t), ["b"]);
    }

    #[test]
    fn deep_and_or_chains() {
        let t = d(2024, 5, 10);
        let items = [it("a", None, &[], 1, None), it("b", None, &[], 2, None)];
        let and = (0..1000).fold(leaf(Leaf::NoLabels), |a, _| {
            FilterAst::And(Box::new(a), Box::new(leaf(Leaf::Priority(1))))
        });
        assert_eq!(evaluate(&and, &items, t), ["a"]);
        let or = (0..1000).fold(leaf(Leaf::Priority(3)), |a, _| {
            FilterAst::Or(Box::new(a), Box::new(leaf(Leaf::Priority(3))))
        });
        assert_eq!(evaluate(&or, &items, t), Vec::<String>::new());
        let or_last = FilterAst::Or(Box::new(or), Box::new(leaf(Leaf::Priority(2))));
        assert_eq!(evaluate(&or_last, &items, t), ["b"]);
    }

    #[test]
    fn boolean_identities_via_string_queries() {
        let t = d(2024, 5, 10);
        let items = [
            it("a", Some("Work"), &["u"], 1, Some("2024-05-10")),
            it("b", Some("Home"), &[], 2, Some("2024-05-01")),
            it("c", None, &[], 4, None),
        ];
        // De Morgan
        assert_eq!(
            ids("!(today | #Home)", &items, t),
            ids("!today & !#Home", &items, t)
        );
        assert_eq!(
            ids("!(today & p1)", &items, t),
            ids("!today | !p1", &items, t)
        );
        assert_eq!(ids("((((p1))))", &items, t), ["a"]);
        // x & !x empty, x | !x everything live
        assert_eq!(ids("today & !today", &items, t), Vec::<String>::new());
        assert_eq!(ids("today | !today", &items, t), ["a", "b", "c"]);
        // precedence: ! > & > |
        assert_eq!(ids("!p1 & p2 | p4", &items, t), ["b", "c"]);
        assert_eq!(ids("p4 | !p1 & p2", &items, t), ["b", "c"]);
        assert_eq!(ids("(p4 | !p1) & p2", &items, t), ["b"]);
    }

    // -- resolve --

    #[test]
    fn resolve_unsupported_reason_is_verbatim_from_parse() {
        let t = d(2024, 5, 10);
        let items = [it("a", Some("Work"), &[], 1, Some("2024-05-10"))];
        let queries = [
            "assigned to: me",
            "search: dentist",
            "due before: Jan 3",
            "#Work & assigned to: alice",
            "(today | overdue",
            "today)",
            "today foo",
            "",
            "   ",
            "!!p1",
            "today &",
            "| today",
            "##Work",
            "/sec",
            "shared",
            "p5",
            "#",
            "@",
            "view all",
            "no",
        ];
        for q in queries {
            let want = parse(q).unwrap_err().reason;
            assert_eq!(
                resolve(q, &items, t),
                FilterResult::Unsupported { reason: want },
                "{q:?}"
            );
        }
    }

    #[test]
    fn resolve_ok_equals_parse_then_evaluate() {
        let t = d(2024, 5, 10);
        let items = [
            it("a", Some("Work"), &["u"], 1, Some("2024-05-10")),
            it("b", Some("Home"), &[], 2, Some("2024-05-01T09:00:00Z")),
        ];
        for q in [
            "today",
            "overdue | p1",
            "!#work",
            "NO DATE",
            "no due date",
            "( p1 )",
        ] {
            let ast = parse(q).unwrap();
            assert_eq!(
                resolve(q, &items, t),
                FilterResult::Ok(evaluate(&ast, &items, t)),
                "{q}"
            );
        }
        // keywords are case-insensitive, names are not
        assert_eq!(ids("TODAY", &items, t), ["a"]);
        assert_eq!(ids("@U", &items, t), Vec::<String>::new());
    }
}
