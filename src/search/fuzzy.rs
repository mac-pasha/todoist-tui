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

/// ASCII fast path for `char::to_lowercase().next().unwrap_or(c)`:
/// for ASCII, lowercasing is a single offset on `A`-`Z` and identity
/// otherwise, so the (expensive) Unicode lowercase iterator is only
/// needed for non-ASCII chars. Behaviour is identical to the slow path for
/// every `char` — this exists purely because `match_score` classifies
/// every needle/haystack char on every call (T-912 hot path) and the
/// Unicode tables dominated the profile once allocation churn was fixed.
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        if c.is_ascii_uppercase() {
            ((c as u8) - b'A' + b'a') as char
        } else {
            c
        }
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// ASCII fast path for `char::is_alphanumeric`, same reasoning as
/// [`fold_char`]. Identical results, cheaper for the common ASCII case.
fn is_alphanumeric_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || (!c.is_ascii() && c.is_alphanumeric())
}

/// ASCII fast path for `char::is_lowercase`, same reasoning as [`fold_char`].
fn is_lowercase_char(c: char) -> bool {
    c.is_ascii_lowercase() || (!c.is_ascii() && c.is_lowercase())
}

/// ASCII fast path for `char::is_uppercase`, same reasoning as [`fold_char`].
fn is_uppercase_char(c: char) -> bool {
    c.is_ascii_uppercase() || (!c.is_ascii() && c.is_uppercase())
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

/// Greedy case-insensitive subsequence test on `fold_char`-folded chars.
///
/// Exactly the predicate the DP decides with `d_score <= NEG` (a
/// subsequence exists iff the DP can align all needle chars), but a
/// single two-pointer pass with no buffers at all. Used as a cheap
/// pre-check in `match_score_in` so non-matching haystacks skip the char
/// tables, word-boundary pass and the entire DP (T-912 hot path).
///
/// The caller must have already handled the empty needle (which trivially
/// matches everything).
fn is_folded_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().map(fold_char);
    'needle: for nch in needle.chars().map(fold_char) {
        for hch in hay.by_ref() {
            if hch == nch {
                continue 'needle;
            }
        }
        return false;
    }
    true
}

/// Fills `buf` with the [`CharInfo`]s for `s`, clearing anything left over
/// from a previous use of the same buffer. Returns the filled prefix as a
/// slice.
///
/// Scratch-buffer twin of [`char_infos`] (see [`MatchScratch`] for why the
/// hot path avoids per-call `Vec` allocation).
fn char_infos_into<'a>(buf: &'a mut Vec<CharInfo>, s: &str) -> &'a [CharInfo] {
    buf.clear();
    buf.reserve(s.chars().count());
    buf.extend(s.char_indices().map(|(byte_start, ch)| CharInfo {
        byte_start,
        byte_len: ch.len_utf8(),
        original: ch,
        folded: fold_char(ch),
    }));
    buf
}

fn is_word_boundary(hay: &[CharInfo], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let prev = hay[index - 1].original;
    let cur = hay[index].original;
    if !is_alphanumeric_char(prev) && is_alphanumeric_char(cur) {
        return true;
    }
    if is_lowercase_char(prev) && is_uppercase_char(cur) {
        return true;
    }
    false
}

/// Fills `buf` with the per-position word-boundary flags for `hay_chars`,
/// reusing the buffer's existing allocation.
///
/// Scratch-buffer twin of the old `boundary` collection in
/// `match_score_in` (see [`MatchScratch`]).
fn fill_boundaries<'a>(buf: &'a mut Vec<bool>, hay_chars: &[CharInfo]) -> &'a [bool] {
    buf.clear();
    buf.resize(hay_chars.len(), false);
    for (j, flag) in buf.iter_mut().enumerate() {
        *flag = is_word_boundary(hay_chars, j);
    }
    buf
}

/// Returns `Some(MatchScore)` when `needle` is a case-insensitive
/// subsequence of `haystack`, `None` otherwise.
///
/// An empty `needle` always matches, with a constant [`EMPTY_NEEDLE_SCORE`]
/// and no matched ranges.
///
/// This allocates a throwaway [`MatchScratch`] for the single call; callers
/// that run many matches in one logical operation (e.g.
/// [`crate::search::query::search`] matching thousands of items per
/// keystroke, T-908/T-912) should instead create one `MatchScratch` and
/// call [`match_score_in`] directly so the DP buffers are reused across
/// calls instead of reallocated on every one.
pub fn match_score(needle: &str, haystack: &str) -> Option<MatchScore> {
    let mut scratch = MatchScratch::new();
    match_score_in(needle, haystack, &mut scratch)
}

/// Same matching/scoring logic as [`match_score`], but takes a caller-owned
/// [`MatchScratch`] so the DP buffers it needs can be reused across many
/// calls (e.g. one per item per keystroke in [`crate::search::query::search`])
/// instead of being freshly allocated every time. See [`MatchScratch`] for
/// why this matters.
pub(crate) fn match_score_in(
    needle: &str,
    haystack: &str,
    scratch: &mut MatchScratch,
) -> Option<MatchScore> {
    // Preprocessing fills scratch's per-char buffers; the DP fill below
    // then reads them back out of the scratch, so the field borrows end
    // before `fill_score_tables` takes the whole struct.
    //
    // Cheap pre-check: while the user is narrowing results, most haystacks
    // are not even a subsequence match, and this two-pointer pass (no
    // buffers, no tables) rejects those far cheaper than running the DP
    // and inspecting `d_score`. A `true` result is inconclusive on score,
    // so the DP still runs.
    if !is_folded_subsequence(needle, haystack) {
        return None;
    }

    {
        let MatchScratch {
            needle_chars: needle_buf,
            hay_chars: hay_buf,
            boundary: boundary_buf,
            ..
        } = scratch;

        let needle_chars = char_infos_into(needle_buf, needle);
        if needle_chars.is_empty() {
            return Some(MatchScore {
                score: EMPTY_NEEDLE_SCORE,
                ranges: Vec::new(),
            });
        }

        let hay_chars = char_infos_into(hay_buf, haystack);
        if needle_chars.len() > hay_chars.len() {
            return None;
        }

        fill_boundaries(boundary_buf, hay_chars);
    }

    fill_score_tables(scratch);

    let n = scratch.needle_chars.len();
    let m = scratch.hay_chars.len();
    let width = scratch.width();
    let final_score = scratch.d_score[n * width + m];
    if final_score <= NEG {
        return None;
    }

    // Backtrack through `d_arg`/`came_from_contiguous` to recover the
    // haystack positions (1-indexed) matched by each needle char.
    scratch.positions.clear();
    scratch.positions.resize(n, 0);
    let mut j = scratch.d_arg[n * width + m];
    for i in (1..=n).rev() {
        scratch.positions[i - 1] = j;
        if i > 1 {
            j = if scratch.came_from_contiguous[i * width + j] {
                j - 1
            } else {
                scratch.d_arg[(i - 1) * width + (j - 1)]
            };
        }
    }

    let ranges = merge_ranges(&scratch.hay_chars, &scratch.positions);

    Some(MatchScore {
        score: final_score,
        ranges,
    })
}

/// Reusable scratch buffers for the dynamic-programming score matrices
/// used by [`match_score_in`].
///
/// [`match_score`] used to allocate four fresh `Vec<Vec<_>>` matrices
/// (`m_score`, `came_from_contiguous`, `d_score`, `d_arg`) on *every* call.
/// `search()` (T-904) calls `match_score` up to twice per item — once for
/// `content`, once for `description` — across up to 5000 items per
/// keystroke, so that is up to 10000 * 4 allocations (plus the inner `Vec`
/// of each row) per keystroke, which measured at 25-90ms p95 in release
/// against a 16ms budget (T-908/T-912).
///
/// `MatchScratch` replaces the four `Vec<Vec<_>>` matrices with four flat
/// `Vec<T>` buffers addressed as `row * width + col`, allocated once and
/// grown (never shrunk) only when a later call needs a bigger matrix than
/// any call seen so far. A caller that keeps one `MatchScratch` alive for
/// the whole `search()` invocation and passes it to [`match_score_in`] for
/// every item pays for the growth at most a handful of times (as the
/// buffers ratchet up to the largest `content`/`description` seen) instead
/// of once per call.
///
/// All matrices are logically `(n + 1) x (m + 1)`, 1-indexed on both axes
/// so index 0 cleanly represents "no needle chars consumed yet" / "before
/// the start of the haystack", same as the old `Vec<Vec<_>>` layout.
#[derive(Default)]
pub(crate) struct MatchScratch {
    /// Needle chars + 1 the DP buffers are currently sized for.
    needle_max: usize,
    /// Haystack chars + 1 the DP buffers are currently sized for (also the
    /// row stride for all four flat buffers).
    hay_max: usize,
    /// `m_score[i * cols + j]`: best score aligning `needle[..i]` to
    /// `haystack[..j]` such that needle char `i` matches haystack char `j`
    /// (1-indexed).
    m_score: Vec<i64>,
    /// `came_from_contiguous[i * cols + j]`: whether that best score
    /// extended a contiguous run from `m_score[(i - 1) * cols + (j - 1)]`
    /// (true) or started a new run after a gap, anchored on
    /// `d_score[(i - 1) * cols + (j - 1)]` (false).
    came_from_contiguous: Vec<bool>,
    /// `d_score[i * cols + j]`: best score aligning `needle[..i]` into
    /// `haystack[..j]` without requiring a match at `j` itself.
    d_score: Vec<i64>,
    /// `d_arg[i * cols + j]`: the haystack position (1-indexed) that
    /// achieved `d_score[i * cols + j]`, used to backtrack.
    d_arg: Vec<usize>,
    /// Reusable buffers for the per-call preprocessing steps, so those do
    /// not allocate per call either (they were the next-largest per-call
    /// allocation source after the DP matrices themselves).
    needle_chars: Vec<CharInfo>,
    hay_chars: Vec<CharInfo>,
    boundary: Vec<bool>,
    positions: Vec<usize>,
}

impl MatchScratch {
    /// Creates an empty scratch; buffers are allocated lazily by
    /// [`Self::ensure_capacity`] and the preprocessing helpers on first
    /// use.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Row stride (`m + 1`) the buffers are currently laid out with. Only
    /// meaningful after [`Self::ensure_capacity`] has been called for the
    /// matrix currently in use.
    fn width(&self) -> usize {
        self.hay_max
    }

    /// Grows the flat buffers so a `(n + 1) x (m + 1)` matrix fits,
    /// reusing the existing allocation (and skipping the resize entirely)
    /// whenever it is already big enough.
    fn ensure_capacity(&mut self, n: usize, m: usize) {
        let rows = n + 1;
        let cols = m + 1;
        let needed = rows * cols;
        if needed > self.m_score.len() {
            self.m_score.resize(needed, NEG);
            self.came_from_contiguous.resize(needed, false);
            self.d_score.resize(needed, NEG);
            self.d_arg.resize(needed, 0);
        }
        self.needle_max = rows;
        self.hay_max = cols;
    }
}

fn fill_score_tables(scratch: &mut MatchScratch) {
    let n = scratch.needle_chars.len();
    let m = scratch.hay_chars.len();

    scratch.ensure_capacity(n, m);
    let width = scratch.hay_max;

    let needle_chars = &scratch.needle_chars;
    let hay_chars = &scratch.hay_chars;
    let boundary = &scratch.boundary;
    let m_score = &mut scratch.m_score;
    let came_from_contiguous = &mut scratch.came_from_contiguous;
    let d_score = &mut scratch.d_score;
    let d_arg = &mut scratch.d_arg;

    // No bulk clearing of the matrix interior: the loop below writes every
    // interior cell before any read can observe it (`m_score` is assigned
    // `NEG` in the no-match branch, `d_score`/`d_arg` are written
    // unconditionally by the carry-forward), so stale data left over from
    // an earlier call on this scratch is never observable. Only the border
    // needs resetting: row 0 and column 0 are *read* (as diagonal / left
    // neighbours) but never *written* by the loop, and because the flat
    // layout's row stride changes between calls of different haystack
    // lengths, a position that is border in this call's layout may have
    // been an interior (written) position in an earlier call's layout.
    for j in 0..=m {
        m_score[j] = NEG;
        // Row 0 is never read by the loop below (needle[..0] is the empty
        // prefix, and the i == 1 gap branch short-circuits before touching
        // `d_score`), but the old per-call initialization defined it as 0
        // ("aligning nothing costs nothing"), so keep that invariant.
        d_score[j] = 0;
    }
    for i in 1..=n {
        let col = i * width;
        m_score[col] = NEG;
        d_score[col] = NEG;
        d_arg[col] = 0;
    }

    for i in 1..=n {
        let needle_folded = needle_chars[i - 1].folded;
        for j in 1..=m {
            let idx = i * width + j;
            let idx_diag = (i - 1) * width + (j - 1);
            let idx_left = idx - 1;

            if needle_folded == hay_chars[j - 1].folded {
                let bonus = if boundary[j - 1] {
                    WORD_BOUNDARY_BONUS
                } else {
                    0
                };

                let contiguous_score = if m_score[idx_diag] > NEG {
                    let prefix_bonus = if j == i { PREFIX_BONUS } else { 0 };
                    Some(
                        m_score[idx_diag]
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
                } else if d_score[idx_diag] > NEG {
                    Some(d_score[idx_diag] + BASE_MATCH_SCORE + bonus)
                } else {
                    None
                };

                let (best, from_contiguous) = match (contiguous_score, gap_score) {
                    (Some(c), Some(g)) if c >= g => (c, true),
                    (Some(c), None) => (c, true),
                    (Some(_) | None, Some(g)) => (g, false),
                    (None, None) => (NEG, false),
                };

                m_score[idx] = best;
                came_from_contiguous[idx] = from_contiguous;
            } else {
                // No match here: `m_score` for this cell is "impossible",
                // same value the old per-call `vec![vec![NEG; ..]; ..]`
                // initialization gave it. Written in place instead of a
                // bulk pre-clear so the buffer can be reused across calls.
                m_score[idx] = NEG;
            }

            // Carry forward the best score for aligning needle[..i] within
            // haystack[..j], whether or not haystack char j participates.
            if d_score[idx_left] >= m_score[idx] {
                d_score[idx] = d_score[idx_left];
                d_arg[idx] = d_arg[idx_left];
            } else {
                d_score[idx] = m_score[idx];
                d_arg[idx] = j;
            }
        }
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
