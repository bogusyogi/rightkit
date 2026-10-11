//! Case folding for free-text matching.
//!
//! [`fold`] applies Unicode default case folding, built on `char::to_lowercase` plus a small
//! table of the full-folding exceptions. It is std-only: there is no dependency on a
//! case-folding or normalisation crate (none of `caseless` or `unicode-normalization` is a
//! dependency of this crate, so none is used).
//!
//! Folding is applied one character at a time and never looks at neighbours, so the result
//! does not depend on context, and the matcher can map folded offsets back to source
//! characters exactly. The folding of haystacks and needles is always this module, so the
//! matcher, its snippets and the filename index agree.
//!
//! What it folds (locale-neutral default):
//! - Everything `char::to_lowercase` maps: `CAFÉ` -> `café`, `ÉCOLE` -> `école`.
//! - **Sharp s:** `ß` and `ẞ` fold to `ss`, so `Straße` matches `STRASSE`.
//! - **Final sigma:** `Σ`, `σ` and `ς` all fold to `σ`, whatever their position in the word.
//! - **Long s and Greek/Latin variants:** `ſ` -> `s`, `µ` (U+00B5) -> `μ`, `ϐ` -> `β`,
//!   `ϑ` -> `θ`, `ϕ` -> `φ`, `ϖ` -> `π`, `ϰ` -> `κ`, `ϱ` -> `ρ`, `ϵ` -> `ε`,
//!   U+0345 and U+1FBE -> `ι`, `ẛ` -> `ṡ`.
//! - **Full-folding expansions:** `ŉ`, `ǰ`, `ΐ`, `ΰ`, `և`, `ẖ`, `ẗ`, `ẘ`, `ẙ`, `ẚ`, and the
//!   Latin ligatures U+FB00..U+FB06 (`ﬁ` -> `fi`, `ﬃ` -> `ffi`, `ﬆ` -> `st`, ...).
//! - `İ` (U+0130) folds to `i` + U+0307 (the Unicode full folding), so `İSTANBUL` matches
//!   `İstanbul` but `istanbul` does not match it.
//!
//! Coverage, stated honestly: this is `CaseFolding.txt` statuses C and F for the characters
//! above and for everything `to_lowercase` maps, not a full copy of the table. Not covered:
//! the Greek characters with ypogegrammeni/prosgegrammeni that fold to two-letter sequences
//! (U+1F80..U+1FFC, for example `ᾈ` -> `ἀι`), the Armenian ligatures U+FB13..U+FB17, and
//! Cherokee (which folds toward uppercase in Unicode). They fold as `to_lowercase` does.
//!
//! Opt-in locale folding: [`FoldOptions::turkic`] selects the Turkish/Azeri mappings
//! (`I` -> `ı`, `İ` -> `i`), so `ISPARTA` matches `ısparta` and `İstanbul` matches `istanbul`.
//! The default is locale-neutral.
//!
//! Limits, by design:
//! - **No Unicode normalisation.** A decomposed accent (`e` + U+0301) does not match a
//!   precomposed one (`é`, U+00E9). Text must use the same normalisation form on both sides.
//!   This crate has no diacritic stripping.
//! - **Length can change.** `ß` becomes two characters, `ﬃ` three.
//!
//! # Versioning
//! Folding is part of the matching contract. [`FOLD_VERSION`] names it, so anything that
//! persists folded text (an index built by a consumer) can store the constant and detect a
//! change with [`check_fold_version`]. Version 1 was `str::to_lowercase` (0.2.1 and
//! earlier); version 2 is this module. This crate itself persists no index: the FTS5
//! predicates in [`crate::sql`] leave folding to SQLite, and [`crate::matcher`] and the
//! filename index fold at query time, in memory.

use std::fmt;

/// Version of the folding contract. Bumped whenever [`fold`] output can change for any input.
/// 1 = `str::to_lowercase` (rightkit-search 0.2.1 and earlier). 2 = Unicode default case
/// folding with the table in this module.
pub const FOLD_VERSION: u32 = 2;

/// Locale options for [`fold_with`]. The default is locale-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct FoldOptions {
    /// Turkish/Azeri folding: `I` -> `ı` and `İ` -> `i` (instead of `I` -> `i` and
    /// `İ` -> `i` + U+0307). Index and query must use the same setting.
    pub turkic: bool,
}

impl FoldOptions {
    /// Locale-neutral folding (the default).
    pub const fn new() -> Self {
        Self { turkic: false }
    }

    /// Turkish/Azeri folding.
    pub const fn turkic() -> Self {
        Self { turkic: true }
    }
}

/// A persisted folded form was built with a different folding contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RebuildNeeded {
    /// The version the stored data was built with.
    pub stored: u32,
    /// The version this build folds with ([`FOLD_VERSION`]).
    pub current: u32,
}

impl fmt::Display for RebuildNeeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "index was folded with version {} but this build folds with version {}: rebuild it",
            self.stored, self.current
        )
    }
}

impl std::error::Error for RebuildNeeded {}

/// Compare the fold version stored beside a persisted index with [`FOLD_VERSION`].
///
/// # Errors
/// [`RebuildNeeded`] when they differ. Rebuild the index; do not query it.
pub fn check_fold_version(stored: u32) -> Result<(), RebuildNeeded> {
    if stored == FOLD_VERSION {
        Ok(())
    } else {
        Err(RebuildNeeded {
            stored,
            current: FOLD_VERSION,
        })
    }
}

/// Full-folding expansions that `char::to_lowercase` does not perform.
fn full(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{00DF}' | '\u{1E9E}' => "ss",
        '\u{0149}' => "\u{02BC}n",
        '\u{01F0}' => "j\u{030C}",
        '\u{0390}' => "\u{03B9}\u{0308}\u{0301}",
        '\u{03B0}' => "\u{03C5}\u{0308}\u{0301}",
        '\u{0587}' => "\u{0565}\u{0582}",
        '\u{1E96}' => "h\u{0331}",
        '\u{1E97}' => "t\u{0308}",
        '\u{1E98}' => "w\u{030A}",
        '\u{1E99}' => "y\u{030A}",
        '\u{1E9A}' => "a\u{02BE}",
        '\u{FB00}' => "ff",
        '\u{FB01}' => "fi",
        '\u{FB02}' => "fl",
        '\u{FB03}' => "ffi",
        '\u{FB04}' => "ffl",
        '\u{FB05}' | '\u{FB06}' => "st",
        _ => return None,
    })
}

/// Single-character folds applied to the result of `to_lowercase` (characters that are
/// already lowercase but fold to another character).
fn simple(c: char) -> char {
    match c {
        '\u{03C2}' => '\u{03C3}', // final sigma -> sigma
        '\u{017F}' => 's',
        '\u{00B5}' => '\u{03BC}',
        '\u{03D0}' => '\u{03B2}',
        '\u{03D1}' => '\u{03B8}',
        '\u{03D5}' => '\u{03C6}',
        '\u{03D6}' => '\u{03C0}',
        '\u{03F0}' => '\u{03BA}',
        '\u{03F1}' => '\u{03C1}',
        '\u{03F5}' => '\u{03B5}',
        '\u{0345}' | '\u{1FBE}' => '\u{03B9}',
        '\u{1E9B}' => '\u{1E61}',
        other => other,
    }
}

fn push_folded(c: char, opts: FoldOptions, out: &mut String) {
    if opts.turkic {
        match c {
            'I' | '\u{0131}' => return out.push('\u{0131}'),
            '\u{0130}' => return out.push('i'),
            _ => {}
        }
    }
    if let Some(s) = full(c) {
        out.push_str(s);
        return;
    }
    for l in c.to_lowercase() {
        out.push(simple(l));
    }
}

/// Byte length of `c` after [`fold_with`]. Folding is per character, so the folded length of
/// a string is the sum of this over its characters.
pub fn folded_len(c: char, opts: FoldOptions) -> usize {
    let mut s = String::new();
    push_folded(c, opts, &mut s);
    s.len()
}

/// Fold `s` with locale-neutral Unicode default case folding. No normalisation is applied.
pub fn fold(s: &str) -> String {
    fold_with(s, FoldOptions::default())
}

/// Fold `s` with the given options. No normalisation is applied.
pub fn fold_with(s: &str, opts: FoldOptions) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_folded(c, opts, &mut out);
    }
    out
}
