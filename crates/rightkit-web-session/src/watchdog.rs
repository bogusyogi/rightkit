//! The action-loop watchdog of an agent browser session. A model that is stuck presses the same
//! control, or two controls in turn, until the session's clock runs out. The watchdog keeps the
//! signatures of the last [`WINDOW`] actions of one session and counts how often the newest one
//! appears in it, or, when the end of the window alternates between two signatures (A, B, A, B),
//! how many actions that run holds, whichever is more.
//!
//! The signature is a hash of the action kind, the target and the address of the page, so the same
//! press on a page that moved on is a new signature. Nothing here keeps text: a typed value is
//! never part of a signature.

use std::collections::VecDeque;

/// Signatures kept.
const WINDOW: usize = 20;
/// Repeats that earn a warning in the answer.
const WARN_AT: usize = 5;
/// Repeats that earn a firm warning that names the owner's live view as the way out.
const ESCALATE_AT: usize = 8;
/// Repeats at which the session refuses further actions.
const STOP_AT: usize = 12;

/// What the newest action brought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing unusual.
    Fine,
    /// The same action or cycle has run this many times.
    Warn(usize),
    /// As `Warn`, and the agent is told to change approach or hand the page to the owner.
    Escalate(usize),
    /// Too many: the action is refused.
    Stop(usize),
}

/// The window of one session.
#[derive(Default)]
pub struct Dog {
    seen: VecDeque<u64>,
}

/// A signature for one action: FNV-1a over its parts, each followed by a separator byte.
pub fn signature(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0x1f)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

impl Dog {
    /// Records one action's signature and says what it brings.
    pub fn note(&mut self, sig: u64) -> Verdict {
        if self.seen.len() == WINDOW {
            self.seen.pop_front();
        }
        self.seen.push_back(sig);
        let same = self.seen.iter().filter(|s| **s == sig).count();
        let repeats = same.max(self.alternation());
        if repeats >= STOP_AT {
            Verdict::Stop(repeats)
        } else if repeats >= ESCALATE_AT {
            Verdict::Escalate(repeats)
        } else if repeats >= WARN_AT {
            Verdict::Warn(repeats)
        } else {
            Verdict::Fine
        }
    }

    /// The length of the run at the end of the window that alternates between the newest two
    /// distinct signatures (A, B, A, B, ...); 0 when that run is shorter than four actions.
    fn alternation(&self) -> usize {
        let n = self.seen.len();
        if n < 4 {
            return 0;
        }
        let a = self.seen[n - 1];
        let b = self.seen[n - 2];
        if a == b {
            return 0;
        }
        let run = self
            .seen
            .iter()
            .rev()
            .enumerate()
            .take_while(|(back, sig)| **sig == if back % 2 == 0 { a } else { b })
            .count();
        if run < 4 { 0 } else { run }
    }

    /// Forgets everything (the owner took the page and handed it back).
    pub fn reset(&mut self) {
        self.seen.clear();
    }
}
