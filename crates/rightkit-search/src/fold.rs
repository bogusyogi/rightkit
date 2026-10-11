//! Case folding for free-text matching.
//!
//! [`fold`] is `str::to_lowercase`, which is Unicode-aware (`CAFÉ` folds to `café`,
//! `ÉCOLE` to `école`). It is std-only: there is no dependency on a normalisation crate.
//!
//! Limits, by design:
//! - **No Unicode normalisation.** A decomposed accent (`e` + U+0301) does not match a
//!   precomposed one (`é`, U+00E9). Text must use the same normalisation form on both sides.
//! - **Length can change.** `İ` (U+0130) folds to `i` + U+0307, so `İSTANBUL` matches
//!   `İstanbul` but `istanbul` does not match it.
//! - **Final sigma.** `str::to_lowercase` maps a word-final `Σ` to `ς`, so a query
//!   spelled with `σ` at the end of a word does not match that word.
//!
//! The folding applied to haystacks and needles is always this function, so the
//! matcher and its snippets agree.

/// Lowercase `s` with `str::to_lowercase`. No normalisation is applied.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}
