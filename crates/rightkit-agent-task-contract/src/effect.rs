//! Effect classification: what counts as irreversible disclosure or publication,
//! and the "no double asking" rule.

use serde::{Deserialize, Serialize};

/// Vocabulary version for [`EffectClass`].
pub const EFFECT_CLASS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectClass {
    /// Undoable by a later action (for example a Git revert of a local commit
    /// the owner already authorized). Covered by grants and standing policy.
    Reversible,
    /// Cannot be recalled. Deletion that cannot be restored.
    IrreversibleDestruction,
    /// Moves money or a financial asset.
    Spend,
    /// Makes non-public material public, or sends private content to a third
    /// party or a service outside the owner's own accounts. Disclosure is classed
    /// here.
    Publication,
}

impl EffectClass {
    /// True for the classes that go to the owner's approval check (asks once,
    /// bound by [`crate::ApprovalBinding`]). Reversible acts are not
    /// asked again when an unexpired grant or standing policy covers them.
    pub const fn is_irreversible(self) -> bool {
        !matches!(self, EffectClass::Reversible)
    }
}
