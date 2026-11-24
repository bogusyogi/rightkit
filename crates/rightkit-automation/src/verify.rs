//! Observe, act, observe again, and say whether anything relevant changed.
//!
//! An action that reports "delivered" says only that the event was accepted. A control that
//! ignores it also reports delivered. This module is the comparison that tells the two apart:
//! take an observation before, act, take one after, and call the result [`Verdict::NoEffect`]
//! when nothing the caller counts as relevant differs.
//!
//! The rule is the same as [`crate::focus::FocusWatch`]: a measurement that could not be taken
//! is never a pass and never a failure of the action. It is [`Verdict::Unmeasured`], and the
//! caller must not report it as an effect. `NoEffect` is only ever the answer when both
//! observations were taken, were complete enough to see a change, and were equal.
//!
//! [`observe_compare`] is the general helper over any observation type. The window-level
//! comparison [`crate::engine::Automation::act_verified`] uses is [`WindowState`].

use crate::engine::ActOutcome;
use crate::model::{Node, Snapshot};

/// What the comparison found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Something relevant differs between the two observations.
    Effect,
    /// Both observations were taken, and nothing relevant differs. The action did nothing the
    /// caller can see.
    NoEffect,
    /// An observation is missing or too incomplete to tell. Neither an effect nor a no-effect.
    Unmeasured,
}

impl Verdict {
    /// `"effect"`, `"no_effect"` or `"unmeasured"`, for receipts and tool answers.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Effect => "effect",
            Verdict::NoEffect => "no_effect",
            Verdict::Unmeasured => "unmeasured",
        }
    }
}

/// How two observations of one thing relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    /// A relevant difference.
    Changed,
    /// Equal in everything relevant, and complete enough that a difference would have shown.
    Same,
    /// Equal as far as seen, but a difference could be hiding (a truncated tree, a field the
    /// platform would not read).
    Unknown,
}

/// Compare two observations. A missing one is `Unmeasured`.
pub fn compare<T>(
    before: Option<&T>,
    after: Option<&T>,
    delta: impl Fn(&T, &T) -> Delta,
) -> Verdict {
    match (before, after) {
        (Some(b), Some(a)) => match delta(b, a) {
            Delta::Changed => Verdict::Effect,
            Delta::Same => Verdict::NoEffect,
            Delta::Unknown => Verdict::Unmeasured,
        },
        _ => Verdict::Unmeasured,
    }
}

/// What [`observe_compare`] saw.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed<T, R> {
    /// What the action returned.
    pub result: R,
    pub before: Option<T>,
    pub after: Option<T>,
    pub verdict: Verdict,
}

/// Observe, run `act`, observe again, and compare with `delta`. `observe` returns `None` when it
/// could not measure. It is called exactly twice, the first time before `act` starts.
pub fn observe_compare<T, R>(
    mut observe: impl FnMut() -> Option<T>,
    act: impl FnOnce() -> R,
    delta: impl Fn(&T, &T) -> Delta,
) -> Observed<T, R> {
    let before = observe();
    let result = act();
    let after = observe();
    let verdict = compare(before.as_ref(), after.as_ref(), delta);
    Observed {
        result,
        before,
        after,
        verdict,
    }
}

type Row = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<bool>,
);

/// The part of a window's accessibility tree an action can change: each node's id, role, name,
/// value and enabled flag, in tree order, plus which node holds focus. Frames, snapshot ids and
/// timestamps are left out, so a window that merely moved is not an effect.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    rows: Vec<Row>,
    focused: Option<String>,
    /// False when the snapshot hit its node or depth limit: a change past the limit is invisible.
    complete: bool,
}

impl WindowState {
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        fn walk(node: &Node, out: &mut Vec<Row>) {
            out.push((
                node.id.clone(),
                node.role.clone(),
                node.name.clone(),
                node.value.clone(),
                node.enabled,
            ));
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut rows = Vec::new();
        walk(&snap.root, &mut rows);
        WindowState {
            rows,
            focused: snap.focused.clone(),
            complete: !snap.truncated,
        }
    }

    /// The window or app is gone, which is itself a change.
    pub fn gone() -> Self {
        WindowState {
            rows: Vec::new(),
            focused: None,
            complete: true,
        }
    }

    /// `Changed` on any difference; `Same` only when both sides are complete.
    pub fn delta(before: &WindowState, after: &WindowState) -> Delta {
        if before.rows != after.rows || before.focused != after.focused {
            Delta::Changed
        } else if before.complete && after.complete {
            Delta::Same
        } else {
            Delta::Unknown
        }
    }
}

/// An act and what [`crate::engine::Automation::act_verified`] observed around it.
#[derive(Debug, Clone)]
pub struct VerifiedOutcome {
    pub outcome: ActOutcome,
    /// `None` when nothing ran (the guard found the snapshot stale), so there is nothing to
    /// verify.
    pub verdict: Option<Verdict>,
}

impl VerifiedOutcome {
    /// The act ran and was not shown to have done nothing. An unmeasured act still counts as
    /// done here; use [`VerifiedOutcome::verified`] to require proof.
    pub fn done(&self) -> bool {
        self.outcome.done() && self.verdict != Some(Verdict::NoEffect)
    }

    /// The act ran and something relevant changed.
    pub fn verified(&self) -> bool {
        self.outcome.done() && self.verdict == Some(Verdict::Effect)
    }

    /// The act ran and nothing relevant changed.
    pub fn no_effect(&self) -> bool {
        self.verdict == Some(Verdict::NoEffect)
    }
}
