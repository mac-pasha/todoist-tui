//! Case-insensitive subsequence fuzzy matching (T-902, EPIC-10).
//!
//! [`match_score`] decides whether `needle` is a case-insensitive
//! subsequence of `haystack` and, if so, returns a [`MatchScore`] carrying a
//! numeric score plus the matched byte ranges in `haystack` (so a later UI
//! task can highlight them without re-running the matcher).
//!
//! The scoring favours the kind of match a human would call "a better fit":
//! a contiguous run of matched characters scores higher than the same
//! characters scattered across the haystack, a match that starts at a word
//! boundary (start of string, after a separator, or a `camelCase` hump)
//! scores higher than one buried mid-word, and a match that aligns with the
//! very start of the haystack (a prefix match) scores highest of all. The
//! algorithm is a single deterministic dynamic-programming pass (no
//! hashing, no iteration order that could vary between runs), so calling
//! [`match_score`] twice with the same arguments always returns the same
//! [`MatchScore`]. Comparing scores from *different* haystacks only breaks
//! ties when both the score and the matched text are identical; callers
//! ranking a list of haystacks should additionally tie-break on
//! `haystack.len()` (shorter first) and then on `haystack` itself (byte
//! order) to get a stable order for a fixed fixture set.
//!
//! Matching operates on `char` boundaries throughout (never raw byte
//! indices), so multi-byte UTF-8 input is handled without panics or
//! byte-boundary slicing; the returned ranges are always valid byte offsets
//! into `haystack` that land on `char` boundaries.

use std::ops::Range;

/// Flat score awarded to every haystack when `needle` is empty.
///
/// An empty needle is a (trivial) subsequence of everything, and every
/// such match is equally good, so this is a constant rather than something
/// derived from the haystack.
const EMPTY_NEEDLE_SCORE: i64 = 0;

/// Score contribution for each matched character, before bonuses.
const BASE_MATCH_SCORE: i64 = 16;

/// Extra score for a matched character that immediately follows the
/// previous matched character in the haystack (no gap between them).
const CONTIGUOUS_BONUS: i64 = 10;

/// Extra score for a matched character that sits at a word boundary in the
/// haystack (start of string, right after a non-alphanumeric separator, or
/// a `camelCase` hump).
const WORD_BOUNDARY_BONUS: i64 = 8;

/// Extra score for a matched character that continues an unbroken run from
/// the very start of the haystack, i.e. the needle (so far) is a literal
/// prefix of the haystack.
const PREFIX_BONUS: i64 = 12;

/// Sentinel for "this alignment is impossible" in the score matrices.
/// Kept well clear of `i64::MIN` so additions never overflow/underflow.
const NEG: i64 = i64::MIN / 4;

/// Result of a successful fuzzy match.
///
/// `score` is only meaningful relative to other [`MatchScore`]s produced by
/// matching against the same needle; higher is a better match. `ranges` are
/// the byte ranges in the haystack that were matched, in ascending,
/// non-overlapping order, suitable for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchScore {
    /// Higher is a better match.
    pub score: i64,
    /// Matched byte ranges in the haystack, ascending and non-overlapping.
    pub ranges: Vec<Range<usize>>,
}

/// Per-`char` view of a string: its byte offset, its original form (used
/// for word-boundary detection) and its case-folded form (used for
/// comparison).
struct CharInfo {
    byte_start: usize,
    byte_len: usize,
    original: char,
    folded: char,
}

fn char_infos(s: &str) -> Vec<CharInfo> {
    s.char_indices()
        .map(|(byte_start, ch)| CharInfo {
            byte_start,
            byte_len: ch.len_utf8(),
            original: ch,
            // `char::to_lowercase` can yield more than one `char` for a
            // handful of Unicode code points; taking the first is enough
            // for case-insensitive comparison purposes here and keeps the
            // needle/haystack char counts independent of each other.
            folded: ch.to_lowercase().next().unwrap_or(ch),
        })
        .collect()
}

fn is_word_boundary(hay: &[CharInfo], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let prev = hay[index - 1].original;
    let cur = hay[index].original;
    if !prev.is_alphanumeric() && cur.is_alphanumeric() {
        return true;
    }
    if prev.is_lowercase() && cur.is_uppercase() {
        return true;
    }
    false
}

/// Returns `Some(MatchScore)` when `needle` is a case-insensitive
/// subsequence of `haystack`, `None` otherwise.
///
/// An empty `needle` always matches, with a constant [`EMPTY_NEEDLE_SCORE`]
/// and no matched ranges.
pub fn match_score(needle: &str, haystack: &str) -> Option<MatchScore> {
    let needle_chars = char_infos(needle);
    if needle_chars.is_empty() {
        return Some(MatchScore {
            score: EMPTY_NEEDLE_SCORE,
            ranges: Vec::new(),
        });
    }

    let hay_chars = char_infos(haystack);
    let n = needle_chars.len();
    let m = hay_chars.len();
    if n > m {
        return None;
    }

    let boundary: Vec<bool> = (0..m).map(|j| is_word_boundary(&hay_chars, j)).collect();

    let tables = build_score_tables(&needle_chars, &hay_chars, &boundary);

    if tables.d_score[n][m] <= NEG {
        return None;
    }

    // Backtrack through `d_arg`/`came_from_contiguous` to recover the
    // haystack positions (1-indexed) matched by each needle char.
    let mut positions = vec![0usize; n];
    let mut j = tables.d_arg[n][m];
    for i in (1..=n).rev() {
        positions[i - 1] = j;
        if i > 1 {
            j = if tables.came_from_contiguous[i][j] {
                j - 1
            } else {
                tables.d_arg[i - 1][j - 1]
            };
        }
    }

    let ranges = merge_ranges(&hay_chars, &positions);

    Some(MatchScore {
        score: tables.d_score[n][m],
        ranges,
    })
}

/// The dynamic-programming score matrices used by [`match_score`], and the
/// backtracking metadata needed to recover the matched haystack positions.
///
/// All matrices are `(n + 1) x (m + 1)`, 1-indexed on both axes so index 0
/// cleanly represents "no needle chars consumed yet" / "before the start of
/// the haystack".
struct ScoreTables {
    /// `m_score[i][j]`: best score aligning `needle[..i]` to `haystack[..j]`
    /// such that needle char `i` matches haystack char `j` (1-indexed).
    m_score: Vec<Vec<i64>>,
    /// `came_from_contiguous[i][j]`: whether that best score extended a
    /// contiguous run from `m_score[i - 1][j - 1]` (true) or started a new
    /// run after a gap, anchored on `d_score[i - 1][j - 1]` (false).
    came_from_contiguous: Vec<Vec<bool>>,
    /// `d_score[i][j]`: best score aligning `needle[..i]` into
    /// `haystack[..j]` without requiring a match at `j` itself.
    d_score: Vec<Vec<i64>>,
    /// `d_arg[i][j]`: the haystack position (1-indexed) that achieved
    /// `d_score[i][j]`, used to backtrack.
    d_arg: Vec<Vec<usize>>,
}

fn build_score_tables(
    needle_chars: &[CharInfo],
    hay_chars: &[CharInfo],
    boundary: &[bool],
) -> ScoreTables {
    let n = needle_chars.len();
    let m = hay_chars.len();

    let mut m_score = vec![vec![NEG; m + 1]; n + 1];
    let mut came_from_contiguous = vec![vec![false; m + 1]; n + 1];
    let mut d_score = vec![vec![NEG; m + 1]; n + 1];
    let mut d_arg = vec![vec![0usize; m + 1]; n + 1];

    if let Some(row) = d_score.first_mut() {
        row.fill(0);
    }

    for i in 1..=n {
        for j in 1..=m {
            if needle_chars[i - 1].folded == hay_chars[j - 1].folded {
                let bonus = if boundary[j - 1] {
                    WORD_BOUNDARY_BONUS
                } else {
                    0
                };

                let contiguous_score = if m_score[i - 1][j - 1] > NEG {
                    let prefix_bonus = if j == i { PREFIX_BONUS } else { 0 };
                    Some(
                        m_score[i - 1][j - 1]
                            + BASE_MATCH_SCORE
                            + bonus
                            + CONTIGUOUS_BONUS
                            + prefix_bonus,
                    )
                } else if i == 1 && j == 1 {
                    // First needle char matching the first haystack char:
                    // trivially contiguous and a prefix match.
                    Some(BASE_MATCH_SCORE + bonus + PREFIX_BONUS)
                } else {
                    None
                };

                let gap_score = if i == 1 {
                    Some(BASE_MATCH_SCORE + bonus)
                } else if d_score[i - 1][j - 1] > NEG {
                    Some(d_score[i - 1][j - 1] + BASE_MATCH_SCORE + bonus)
                } else {
                    None
                };

                let (best, from_contiguous) = match (contiguous_score, gap_score) {
                    (Some(c), Some(g)) if c >= g => (c, true),
                    (Some(c), None) => (c, true),
                    (Some(_) | None, Some(g)) => (g, false),
                    (None, None) => (NEG, false),
                };

                m_score[i][j] = best;
                came_from_contiguous[i][j] = from_contiguous;
            }

            // Carry forward the best score for aligning needle[..i] within
            // haystack[..j], whether or not haystack char j participates.
            if d_score[i][j - 1] >= m_score[i][j] {
                d_score[i][j] = d_score[i][j - 1];
                d_arg[i][j] = d_arg[i][j - 1];
            } else {
                d_score[i][j] = m_score[i][j];
                d_arg[i][j] = j;
            }
        }
    }

    ScoreTables {
        m_score,
        came_from_contiguous,
        d_score,
        d_arg,
    }
}

/// Turns 1-indexed matched haystack positions into merged, ascending byte
/// ranges (adjacent matched chars collapse into a single range).
fn merge_ranges(hay_chars: &[CharInfo], positions: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &pos in positions {
        let info = &hay_chars[pos - 1];
        let start = info.byte_start;
        let end = info.byte_start + info.byte_len;
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_needle_matches_every_haystack_with_constant_score() {
        let a = match_score("", "anything").expect("empty needle always matches");
        let b = match_score("", "").expect("empty needle matches empty haystack");
        let c = match_score("", "café ☕ naïve").expect("empty needle matches unicode haystack");
        assert_eq!(a.score, EMPTY_NEEDLE_SCORE);
        assert_eq!(a.ranges, [] as [Range<usize>; 0]);
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn rejects_non_subsequence() {
        assert!(match_score("xyz", "abc").is_none());
        assert!(match_score("abcd", "abc").is_none());
    }

    #[test]
    fn case_insensitive_subsequence_matches() {
        let m = match_score("abc", "XaXbXc").expect("abc is a subsequence");
        assert_eq!(m.ranges.len(), 3);
        let m_upper = match_score("ABC", "xaxbxc").expect("case-insensitive match");
        assert_eq!(m.score, m_upper.score);
    }

    #[test]
    fn contiguous_run_scores_higher_than_scattered_match() {
        // Neither haystack has separators or camelCase humps, so the
        // word-boundary bonus stays out of the comparison and this
        // isolates the contiguous-run bonus.
        let contiguous = match_score("abc", "xxabcxx").expect("contiguous match");
        let scattered = match_score("abc", "xaxbxcxx").expect("scattered match");
        assert!(contiguous.score > scattered.score);
    }

    #[test]
    fn word_boundary_match_scores_higher_than_mid_word() {
        let boundary = match_score("foo", "bar_foo").expect("boundary match");
        let mid_word = match_score("foo", "xxfooyy")
            .or_else(|| match_score("foo", "xxxfooxxx"))
            .expect("mid-word match");
        assert!(boundary.score >= mid_word.score);
    }

    #[test]
    fn prefix_match_scores_highest() {
        let prefix = match_score("foo", "foobar").expect("prefix match");
        let non_prefix = match_score("foo", "xfoobar").expect("non-prefix match");
        assert!(prefix.score > non_prefix.score);
    }

    #[test]
    fn ranges_cover_matched_bytes_and_merge_contiguous_runs() {
        let m = match_score("ab", "xxabxx").expect("contiguous match");
        assert_eq!(m.ranges, vec![2..4]);
    }

    #[test]
    fn unicode_haystack_and_needle_match_without_panicking() {
        // Multi-byte haystack with accented and combined characters; also
        // exercises a multi-byte needle.
        let m = match_score("café", "le café ☕ du matin").expect("unicode subsequence match");
        assert_ne!(m.ranges, [] as [Range<usize>; 0]);
        for range in &m.ranges {
            assert!("le café ☕ du matin".is_char_boundary(range.start));
            assert!("le café ☕ du matin".is_char_boundary(range.end));
        }

        // Non-ASCII needle entirely absent from the haystack.
        assert!(match_score("日本語", "le café ☕ du matin").is_none());

        // Non-ASCII needle present as a case-insensitive subsequence.
        let kana = match_score("ネコ", "どうもありがとうネコさん").expect("katakana match");
        assert_ne!(kana.ranges, [] as [Range<usize>; 0]);
    }

    #[test]
    fn repeated_calls_are_deterministic() {
        let first = match_score("tod", "Todoist Task");
        let second = match_score("tod", "Todoist Task");
        assert_eq!(first, second);
    }
}

#[cfg(test)]
mod adversarial_tests {
    use super::*;

    fn matched_text(hay: &str, m: &MatchScore) -> String {
        m.ranges.iter().map(|r| &hay[r.clone()]).collect()
    }

    fn assert_ranges_valid(needle: &str, hay: &str, m: &MatchScore) {
        let mut prev_end = 0;
        for r in &m.ranges {
            assert!(r.start < r.end, "empty range {r:?}");
            assert!(r.end <= hay.len(), "out of bounds {r:?}");
            assert!(hay.is_char_boundary(r.start) && hay.is_char_boundary(r.end));
            assert!(r.start >= prev_end, "overlap/unsorted {:?}", m.ranges);
            prev_end = r.end;
        }
        let got: Vec<char> = matched_text(hay, m)
            .chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect();
        let want: Vec<char> = needle
            .chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect();
        assert_eq!(got, want, "ranges don't spell needle");
    }

    #[test]
    fn ranges_spell_needle_across_fixtures() {
        let hays = [
            "Todoist Task",
            "le café ☕ du matin",
            "どうもありがとうネコさん",
            "ÀÉÎõü straße",
            "a😀b😀c",
            "fooBarBaz_qux-quux",
            "",
            "aaaaaaaa",
        ];
        let needles = [
            "", "a", "abc", "tt", "café", "ネコ", "ß", "😀😀", "fbb", "aaa", "éî", "zzz",
        ];
        for h in hays {
            for n in needles {
                if let Some(m) = match_score(n, h) {
                    assert_ranges_valid(n, h, &m);
                }
            }
        }
    }

    #[test]
    fn emoji_ranges_are_exact() {
        let m = match_score("bc", "a😀b😀c").expect("match");
        assert_eq!(m.ranges, vec![5..6, 10..11]);
    }

    #[test]
    fn unicode_case_insensitive() {
        assert!(match_score("CAFÉ", "café").is_some());
        assert!(match_score("café", "CAFÉ").is_some());
        assert!(match_score("é", "e").is_none());
    }

    #[test]
    fn needle_longer_than_haystack_is_none() {
        assert!(match_score("abc", "").is_none());
        assert!(match_score("日本", "日").is_none());
    }

    #[test]
    fn order_matters_for_subsequence() {
        assert!(match_score("ba", "ab").is_none());
        assert!(match_score("aa", "a").is_none());
        assert!(match_score("aa", "aXa").is_some());
    }

    #[test]
    fn empty_haystack_nonempty_needle_none() {
        assert!(match_score("a", "").is_none());
    }

    #[test]
    fn prefix_beats_boundary_beats_midword() {
        let prefix = match_score("foo", "foobar").expect("m").score;
        let boundary = match_score("foo", "bar_foo").expect("m").score;
        let mid = match_score("foo", "barxfoo").expect("m").score;
        assert!(prefix > boundary, "{prefix} {boundary}");
        assert!(boundary > mid, "{boundary} {mid}");
    }

    #[test]
    fn camel_case_hump_counts_as_boundary() {
        let hump = match_score("b", "fooBar").expect("m").score;
        let mid = match_score("b", "foobar").expect("m").score;
        assert!(hump > mid);
    }

    #[test]
    fn picks_best_alignment_not_first() {
        // "ab" occurs scattered early and contiguous later; contiguous wins.
        let m = match_score("ab", "xaxb_ab").expect("m");
        assert_eq!(m.ranges, vec![5..7]);
        assert_ranges_valid("ab", "xaxb_ab", &m);
    }

    #[test]
    fn identical_inputs_equal_and_score_case_independent() {
        assert_eq!(match_score("Foo", "fooBar"), match_score("fOO", "fooBar"));
    }

    #[test]
    fn stable_ranking_with_documented_tiebreak() {
        let fixtures = [
            "foo_bar", "foobar", "fooxbar", "bar_foo", "Foo", "foo", "afoo",
        ];
        let rank = |order: &[&str]| {
            let mut v: Vec<(i64, usize, String)> = order
                .iter()
                .filter_map(|h| {
                    match_score("foo", h).map(|m| (-m.score, h.len(), (*h).to_string()))
                })
                .collect();
            v.sort();
            v.into_iter().map(|t| t.2).collect::<Vec<_>>()
        };
        let a = rank(&fixtures);
        let mut rev = fixtures;
        rev.reverse();
        assert_eq!(a, rank(&rev));
        assert_eq!(a.len(), fixtures.len());
    }

    #[test]
    fn long_inputs_do_not_panic() {
        let hay = "ab".repeat(500);
        let m = match_score(&"ab".repeat(50), &hay).expect("m");
        assert_ranges_valid(&"ab".repeat(50), &hay, &m);
    }

    #[test]
    fn special_lowercase_expansion_no_panic() {
        // 'İ' lowercases to multiple chars; must not panic or mis-slice.
        let h = "İstanbul";
        if let Some(m) = match_score("i", h) {
            assert_ranges_valid("i", h, &m);
        }
        let _ = match_score("İ", "i");
    }
}
