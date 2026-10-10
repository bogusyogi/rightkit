//! Pure insertion-proof and continuity rules, ported from the HeardRight
//! platform layer (brand-neutral). An insertion is confirmed only by an
//! attributable delta between two reads of the same representation of the
//! same retained editor; a successful setter or posted key is never proof.

/// Which representation produced a text read. A before/after pair must use the
/// same one (AX value vs. text-marker ranges disagree on rich editors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSource {
    Value,
    TextMarker,
}

/// One bounded read of an editor's text and selection (UTF-16 location/length).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FieldText {
    pub text: String,
    pub source: Option<TextSource>,
    pub selection: Option<(usize, usize)>,
}

pub const MAX_PROOF_BYTES: usize = 1_000_000;

pub fn occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        haystack.matches(needle).count()
    }
}

/// Rich editors store a pasted blank-line paragraph break as two blocks and
/// report it back as ONE line break; only runs of line breaks collapse.
pub fn collapse_line_break_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_break = false;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            if !in_break {
                out.push('\n');
            }
            in_break = true;
        } else {
            in_break = false;
            out.push(ch);
        }
    }
    out
}

/// Electron mirrors one logical value exactly twice in AX. Only an exact
/// doubled value collapses; unequal halves never do.
pub fn collapse_exact_mirror(value: &str) -> Option<&str> {
    let mid = value.len() / 2;
    (value.len().is_multiple_of(2) && value.is_char_boundary(mid))
        .then(|| value.split_at(mid))
        .filter(|(l, r)| !l.is_empty() && l == r)
        .map(|(l, _)| l)
}

fn confirm_representation(
    before: &str,
    range: Option<(usize, usize)>,
    after: &str,
    needle: &str,
) -> bool {
    if after == before {
        return false;
    }
    if let Some((location, length)) = range {
        let units: Vec<u16> = before.encode_utf16().collect();
        let end = location.saturating_add(length);
        if end <= units.len() {
            let head = String::from_utf16_lossy(&units[..location]);
            let tail = String::from_utf16_lossy(&units[end..]);
            if after == format!("{head}{needle}{tail}") {
                return true;
            }
        }
    }
    // Placeholder-like values can make the range lie; one new exact occurrence
    // of the text is still proof of this insertion.
    occurrences(after, needle) == occurrences(before, needle) + 1
}

fn confirm_any_representation(
    before: &str,
    range: Option<(usize, usize)>,
    after: &str,
    needle: &str,
) -> bool {
    if confirm_representation(before, range, after, needle) {
        return true;
    }
    let nb = collapse_exact_mirror(before).unwrap_or(before);
    let na = collapse_exact_mirror(after).unwrap_or(after);
    let mirrored = nb != before || na != after;
    (mirrored && confirm_representation(nb, range, na, needle))
        || (mirrored && occurrences(after, needle) == occurrences(before, needle) + 2)
}

/// The single insertion-confirmation rule shared by every tier and platform.
pub fn confirm_insertion(
    before: &str,
    range: Option<(usize, usize)>,
    after: &str,
    needle: &str,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    if confirm_any_representation(before, range, after, needle) {
        return true;
    }
    needle.contains('\n')
        && confirm_any_representation(
            &collapse_line_break_runs(before),
            None,
            &collapse_line_break_runs(after),
            &collapse_line_break_runs(needle),
        )
}

/// Stable fingerprint of an editor's text (after canonical line breaks): FNV-1a
/// of the collapsed text plus its length. Kept instead of the text itself.
pub fn text_fingerprint(text: &str) -> u64 {
    let t = collapse_line_break_runs(text);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in t.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h ^ (t.len() as u64).rotate_left(17)
}

/// Spec 5.1 rule 3 (HeardRight, locked): a replaced editor may receive the text
/// only when it is provably the same box: the original is positively **gone**
/// (a slow or failed read is not "gone"), same process and window, the candidate
/// is positively non-secure, and it holds exactly the text fingerprinted at pin
/// time. A flaky read must never redirect text.
pub fn replacement_editor_may_receive(
    original_gone: bool,
    same_process_and_window: bool,
    candidate_secure: Option<bool>,
    candidate_fingerprint: Option<u64>,
    pinned_fingerprint: Option<u64>,
) -> bool {
    original_gone
        && same_process_and_window
        && candidate_secure == Some(false)
        && candidate_fingerprint.is_some()
        && candidate_fingerprint == pinned_fingerprint
}

/// Continuity of a captured focus anchor measured against an OS-clock
/// observer: a later focus change, a late-started observer, or a stalled
/// observer can never prove the original editor is still focused.
pub fn continuity_window_valid(
    captured_at: u64,
    now: u64,
    coverage_started: u64,
    last_change: u64,
    last_progress: u64,
    max_staleness: u64,
) -> bool {
    captured_at != 0
        && coverage_started != 0
        && coverage_started <= captured_at
        && captured_at <= now
        && last_change <= captured_at
        && last_progress >= captured_at
        && last_progress <= now
        && now - last_progress <= max_staleness
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_break_runs_collapse_only() {
        assert_eq!(collapse_line_break_runs("a\n\nb\r\n\r\nc d"), "a\nb\nc d");
        assert_eq!(collapse_line_break_runs("a  b"), "a  b");
    }

    #[test]
    fn confirmation_requires_attributable_delta() {
        assert!(confirm_insertion("", None, "hello", "hello"));
        let needle = "one.\n\ntwo.";
        assert!(confirm_insertion("\n", Some((0, 0)), "one.\ntwo.", needle));
        assert!(confirm_insertion(
            "draft",
            Some((5, 0)),
            "draftone.\ntwo.",
            needle
        ));
        assert!(confirm_insertion("", None, "one.\ntwo.one.\ntwo.", needle));
        assert!(confirm_insertion(
            "draftdraft",
            None,
            "draftdraftone.\ntwo.one.\ntwo.",
            needle
        ));
        assert!(!confirm_insertion("one.\ntwo.", None, "one.\ntwo.", needle));
        assert!(!confirm_insertion(
            "draft",
            None,
            "draftone.\nother.",
            needle
        ));
        assert!(!confirm_insertion(
            "draft",
            None,
            "draftone.\ntwo.one.\ntwo.",
            needle
        ));
        assert!(!confirm_insertion("hello", None, "hello", "hello"));
        assert!(!confirm_insertion("draft", None, "draft and more", "draft"));
        assert!(!confirm_insertion("gamma gamma", None, "gamma", "gamma"));
        assert!(!confirm_insertion("a", None, "ab", ""));
    }

    #[test]
    fn selection_replacement_is_exact() {
        // "hello world" with "world" (6,5) selected, replaced by "there".
        assert!(confirm_insertion(
            "hello world",
            Some((6, 5)),
            "hello there",
            "there"
        ));
        // Appended instead of replacing: the range reconstruction fails but a
        // single new occurrence still attributes the text.
        assert!(confirm_insertion(
            "hello world",
            Some((6, 5)),
            "hello worldthere",
            "there"
        ));
    }

    #[test]
    fn utf16_ranges_are_respected() {
        assert!(confirm_insertion("é😀x", Some((3, 0)), "é😀Zx", "Z"));
    }

    #[test]
    fn only_the_same_place_with_the_same_text_may_replace_the_original() {
        let fp = Some(text_fingerprint("draft\n\ntext"));
        assert_eq!(
            text_fingerprint("a\n\nb"),
            text_fingerprint("a\nb"),
            "canonical line breaks"
        );
        assert!(replacement_editor_may_receive(
            true,
            true,
            Some(false),
            fp,
            fp
        ));
        assert!(
            !replacement_editor_may_receive(false, true, Some(false), fp, fp),
            "original not provably gone"
        );
        assert!(
            !replacement_editor_may_receive(true, false, Some(false), fp, fp),
            "other process or window"
        );
        assert!(
            !replacement_editor_may_receive(true, true, None, fp, fp),
            "secure state unknown"
        );
        assert!(
            !replacement_editor_may_receive(true, true, Some(true), fp, fp),
            "secure"
        );
        assert!(
            !replacement_editor_may_receive(
                true,
                true,
                Some(false),
                Some(text_fingerprint("other")),
                fp
            ),
            "different text"
        );
        assert!(
            !replacement_editor_may_receive(true, true, Some(false), None, None),
            "nothing fingerprinted"
        );
    }

    #[test]
    fn mirror_collapse_needs_exact_halves() {
        assert_eq!(collapse_exact_mirror("abab"), Some("ab"));
        assert_eq!(collapse_exact_mirror("abac"), None);
        assert_eq!(collapse_exact_mirror(""), None);
    }

    #[test]
    fn separate_process_observers_can_cover_same_stop_timestamp() {
        assert!(continuity_window_valid(100, 200, 10, 90, 190, 30));
        assert!(continuity_window_valid(100, 200, 50, 95, 190, 30));
    }

    #[test]
    fn later_focus_change_and_stalled_observer_invalidate() {
        assert!(!continuity_window_valid(100, 200, 10, 110, 190, 30));
        assert!(!continuity_window_valid(100, 200, 110, 90, 190, 30));
        assert!(!continuity_window_valid(100, 200, 10, 90, 150, 30));
        assert!(!continuity_window_valid(100, 200, 0, 90, 190, 30));
        assert!(!continuity_window_valid(0, 200, 10, 90, 190, 30));
        assert!(!continuity_window_valid(100, 200, 10, 90, 99, 120));
    }
}
