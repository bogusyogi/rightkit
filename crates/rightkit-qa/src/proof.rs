//! An assertion log for release-claim journeys: every check panics with the expected value and its
//! source on a mismatch, and writes one evidence note on a match, so the receipt shows what was
//! compared with what, not only that nothing panicked.
//!
//! Each assertion carries a `src` string: where the expected value comes from (a `file:line` of the
//! source that renders it). The host passes the expected strings and their sources; nothing here knows
//! a product's copy.

use crate::harness::Scenario;
use crate::live_app::norm;

/// An assertion log bound to one [`Scenario`]'s evidence notes.
pub struct Proof<'a> {
    sc: &'a Scenario,
    checks: usize,
}

impl<'a> Proof<'a> {
    /// A log that writes its notes into `sc`.
    pub fn new(sc: &'a Scenario) -> Self {
        Proof { sc, checks: 0 }
    }

    /// Number of assertions that held so far.
    pub fn checks(&self) -> usize {
        self.checks
    }

    fn ok(&mut self, what: &str, detail: String, src: &str) {
        self.checks += 1;
        self.sc.note(format!("PROVEN {what} = {detail} [{src}]"));
    }

    /// `got == want`.
    pub fn eq(&mut self, what: &str, got: &str, want: &str, src: &str) {
        assert_eq!(
            got, want,
            "{what}: the source renders {want:?} ({src}); the live app shows {got:?}"
        );
        self.ok(what, format!("{want:?}"), src);
    }

    /// Whitespace-normalised, case-sensitive containment.
    pub fn has(&mut self, what: &str, haystack: &str, needle: &str, src: &str) {
        let hay = norm(haystack);
        assert!(
            hay.contains(needle),
            "{what}: the source renders {needle:?} ({src}); the live app shows {hay:?}"
        );
        self.ok(what, format!("contains {needle:?}"), src);
    }

    /// The text does not contain `needle` (whitespace-normalised, case-sensitive).
    pub fn lacks(&mut self, what: &str, haystack: &str, needle: &str, src: &str) {
        let hay = norm(haystack);
        assert!(
            !hay.contains(needle),
            "{what}: {needle:?} must not appear ({src}); the live app shows {hay:?}"
        );
        self.ok(what, format!("does not contain {needle:?}"), src);
    }

    /// A boolean state read from the live app; `shown` is the raw reading, printed on failure.
    pub fn yes(&mut self, what: &str, state: bool, shown: &str, src: &str) {
        assert!(state, "{what} ({src}); live state: {shown}");
        self.ok(what, "true".to_string(), src);
    }

    /// Something this journey did not prove. Never an assertion: it is written to the evidence so the
    /// receipt cannot be read as covering it.
    pub fn not_proven(&mut self, what: &str) {
        self.sc.note(format!("NOT PROVEN {what}"));
    }
}
