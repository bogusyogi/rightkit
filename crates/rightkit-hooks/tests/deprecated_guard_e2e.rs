//! rightkit#1687: `DestructiveGuard` is deprecated in favour of `rightkit-agent-policy`, with no
//! behaviour change in the patch release. A host still compiling against it gets the same
//! decisions as before until the type is removed in the next minor release.
#![cfg(feature = "agent_hooks")]
#![allow(deprecated)]

use rightkit_hooks::agent_hooks::{DestructiveGuard, DestructiveGuardDecision};

#[test]
fn deprecated_guard_keeps_its_decisions() {
    let guard = DestructiveGuard::default();
    assert!(matches!(
        guard.evaluate_shell("rm -rf /"),
        DestructiveGuardDecision::Block { .. }
    ));
    assert!(matches!(
        guard.evaluate_shell("git reset --hard"),
        DestructiveGuardDecision::RequireConfirmation { .. }
    ));
    assert_eq!(
        guard.evaluate_shell("ls -la"),
        DestructiveGuardDecision::Allow
    );
    let custom = DestructiveGuard::new().block_pattern("Shred");
    assert!(matches!(
        custom.evaluate_shell("shred notes.txt"),
        DestructiveGuardDecision::Block { .. }
    ));
}
