//! Sequence focus restoration after provider reads and the final window check.

/// What `run_effect` did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EffectRun<P, R, E> {
    /// A precondition read failed or refused. Nothing was claimed and the
    /// effect call was never made.
    Refused(E),
    /// The `claim` closure returned false: the supervisor gate denied it
    /// (abandoned, closed, deadline, caller gone) or the window re-check that
    /// follows the gate inside the closure refused. The effect call was never
    /// made. The prepared value comes back so the CALLER
    /// decides how its COM references are disposed of (release, or leak when
    /// the generation is no longer live); it is not dropped here.
    NotClaimed(P),
    /// Claimed, then the single effect call ran and returned `R`. From here the
    /// effect counts as issued whatever `R` says.
    Issued(R),
}

/// `pre` (all provider reads) -> `claim` (the supervisor's gate) -> `effect`
/// (one provider call). `claim` and `effect` are called back to back: this
/// function adds nothing between them, and `effect` receives only what `pre`
/// already produced, so it has no reason to read anything.
pub(crate) fn run_effect<P, R, E>(
    pre: impl FnOnce() -> Result<P, E>,
    claim: impl FnOnce() -> bool,
    effect: impl FnOnce(P) -> R,
) -> EffectRun<P, R, E> {
    let prepared = match pre() {
        Ok(prepared) => prepared,
        Err(refusal) => return EffectRun::Refused(refusal),
    };
    if !claim() {
        return EffectRun::NotClaimed(prepared);
    }
    EffectRun::Issued(effect(prepared))
}
