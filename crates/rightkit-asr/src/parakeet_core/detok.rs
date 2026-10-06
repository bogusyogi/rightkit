//! [DROP-IN CORE AFTER LOCAL 62ecdfdd IS PRESENT]
//!
//! Canonical token-aware transcript reconstruction. This ports the guarded digit
//! behavior already present in HeardRight's vendored Windows decoder and intended
//! by local macOS commit 62ecdfdd. It also supports simple SentencePiece-only
//! reconstruction so each platform can first reproduce its legacy behavior before
//! any cross-platform behavior reconciliation.

use super::types::{DecodedPiece, TimedToken, TranscriptionResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetokPolicy {
    /// Repair SentencePiece's missing boundary before bare numeric pieces after a
    /// word: `▁at`, `6`, `0` -> `at 60`; preserve `A4` and multi-digit continuation.
    pub guarded_digit_boundaries: bool,
    /// Skip angle-bracket special pieces (`<...>`) except `<unk>`, matching the
    /// vendored Rust decoder's current behavior.
    pub skip_special_tokens: bool,
    /// N0: drop the hesitation words `uh` and `um` from the PIECE stream, before
    /// any text exists. See `strip_mandatory_fillers`.
    pub strip_fillers: bool,
}

impl DetokPolicy {
    /// Intended first-party behavior for macOS after 62ecdfdd and current Windows.
    pub const fn canonical_rust() -> Self {
        Self {
            guarded_digit_boundaries: true,
            skip_special_tokens: true,
            strip_fillers: true,
        }
    }

    /// Simple SentencePiece joining with configurable special-token behavior.
    /// Use this to reproduce a platform's legacy behavior before unifying it.
    pub const fn sentencepiece_only(skip_special_tokens: bool) -> Self {
        Self {
            guarded_digit_boundaries: false,
            skip_special_tokens,
            // Eval harnesses score against references that CONTAIN the fillers,
            // so stripping them here would read as deletions and make the
            // measured WER worse for a product improvement. Legacy reproduction
            // and scoring keep the raw stream.
            strip_fillers: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Detokenized {
    pub text: String,
    /// Display pieces in one-to-one order with non-skipped input pieces. Leading
    /// spaces are retained so timestamped callers can preserve existing token text.
    pub display_pieces: Vec<String>,
}

/// Public so platform adapters that keep their own per-piece side data (timings,
/// durations) can drop exactly the pieces `assemble_pieces` drops and stay in
/// lockstep with its `display_pieces` ordering.
pub fn is_skippable_special(piece: &str, policy: DetokPolicy) -> bool {
    policy.skip_special_tokens
        && piece.starts_with('<')
        && piece.ends_with('>')
        && piece != "<unk>"
}

fn maybe_guard_digit_boundary(full_text: &str, display: &mut String, policy: DetokPolicy) {
    if !policy.guarded_digit_boundaries
        || full_text.is_empty()
        || display.is_empty()
        || display.starts_with(' ')
        || !display.chars().all(|c| c.is_ascii_digit())
    {
        return;
    }

    // This deliberately mirrors the existing vendored Rust rule:
    // * word + digit => insert a boundary (`at60` -> `at 60`)
    // * lowercase article `a` + digit => insert (`a24` -> `a 24`)
    // * single uppercase letter + digit => keep identifier (`A4`)
    // * digit + digit => keep number (`1` + `3` -> `13`)
    let trailing_letters = full_text
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .count();
    let last_char = full_text.chars().last();
    let is_article_a = trailing_letters == 1 && last_char == Some('a');

    if trailing_letters > 1 || is_article_a {
        display.insert(0, ' ');
    }
}

pub fn assemble_pieces<I, S>(pieces: I, policy: DetokPolicy) -> Detokenized
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut full_text = String::new();
    let mut display_pieces = Vec::new();

    for piece in pieces {
        let piece = piece.as_ref();
        if is_skippable_special(piece, policy) {
            continue;
        }

        let mut display = piece.replace('\u{2581}', " ");
        maybe_guard_digit_boundary(&full_text, &mut display, policy);
        full_text.push_str(&display);
        display_pieces.push(display);
    }

    Detokenized {
        text: full_text.trim().to_string(),
        display_pieces,
    }
}

/// Detokenize while preserving per-piece timings. This is the replacement target
/// for macOS production's duplicated `transcribe_pieces_timed` display assembly.
/// N0: hesitation removal at the PIECE level, upstream of every text layer.
///
/// This is the ONLY place hesitations are removed. L0 must not strip them
/// again from the deterministic text with a separate word list: L1 is
/// deliberately fed RAW ASR so its semantic decisions are not constrained by
/// deterministic edits, so a second removal site would leave L1 seeing every
/// `uh` and `um` while creating duplicate logic. Removing them at the piece
/// level covers both layers with one word list.
///
/// Scoped to `uh` and `um` ON PURPOSE. Those are the only hesitations the
/// canonical corpus actually contains (uh 733, um 60); `hmm`, `mmm` and `ah`
/// occur zero times, and each is a real word in some contexts ("hmm" as
/// considered agreement, "ah" as realisation), so removing them would be an
/// unmeasured guess. The old L0 list included `hmm`, `uhh`, `umm`, `erm`, `uhm`
/// and `mmm`; none of them was ever measured, and they are no longer stripped.
///
/// Works on whole words: pieces are grouped from one `▁` boundary to the next,
/// so a group is a word plus any punctuation that trails it. Dropping the whole
/// group takes the filler's bracketing comma with it — `"Uh, hello"` becomes
/// `"hello"` rather than `", hello"` — and leaves the next word's own `▁`
/// intact, so spacing needs no repair.
pub const MANDATORY_FILLERS: [&str; 2] = ["uh", "um"];

/// True when a whole word, ignoring case and surrounding punctuation, is one of
/// [`MANDATORY_FILLERS`]. Shared so the piece-level and text-level entry points
/// can never drift into two different word lists — which is exactly what went
/// wrong when L0 kept its own, wider list.
fn is_mandatory_filler(word: &str) -> bool {
    let normalised = word
        .replace('\u{2581}', "")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    MANDATORY_FILLERS.contains(&normalised.as_str())
}

/// Text entry point for N0, for backends that produce no token stream.
///
/// Whisper returns a bare string, so there are no pieces to filter; it still
/// must not hand `uh`/`um` to L1. Same word list, same whole-word rule, applied
/// at the backend's output rather than in a text layer — so N0 remains one
/// policy with one list, not a per-layer regex.
pub fn strip_mandatory_fillers_text(text: &str) -> String {
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|word| !is_mandatory_filler(word))
        .collect();
    kept.join(" ")
}

pub fn strip_mandatory_fillers(pieces: &[DecodedPiece]) -> Vec<DecodedPiece> {

    let mut out: Vec<DecodedPiece> = Vec::with_capacity(pieces.len());
    let mut group: Vec<&DecodedPiece> = Vec::new();

    let flush = |group: &mut Vec<&DecodedPiece>, out: &mut Vec<DecodedPiece>| {
        if group.is_empty() {
            return;
        }
        let word: String = group.iter().map(|p| p.piece.as_str()).collect();
        if !is_mandatory_filler(&word) {
            out.extend(group.iter().map(|p| (*p).clone()));
        }
        group.clear();
    };

    for piece in pieces {
        if piece.piece.starts_with('\u{2581}') {
            flush(&mut group, &mut out);
        }
        group.push(piece);
    }
    flush(&mut group, &mut out);
    out
}

pub fn assemble_timed_pieces(pieces: &[DecodedPiece], policy: DetokPolicy) -> TranscriptionResult {
    let stripped;
    let pieces = if policy.strip_fillers {
        stripped = strip_mandatory_fillers(pieces);
        &stripped[..]
    } else {
        pieces
    };
    let assembled = assemble_pieces(pieces.iter().map(|p| p.piece.as_str()), policy);
    let tokens = pieces
        .iter()
        .filter(|p| !is_skippable_special(&p.piece, policy))
        .zip(assembled.display_pieces)
        .map(|(p, text)| TimedToken {
            text,
            start: p.start,
            end: p.end,
        })
        .collect();

    TranscriptionResult {
        text: assembled.text,
        tokens,
    }
}

