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
