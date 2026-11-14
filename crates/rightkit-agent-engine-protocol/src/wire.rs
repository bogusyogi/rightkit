//! Process-wide wire shape for the fields that two hosts write differently.
//!
//! The default ([`WireShape::Standard`]) writes exactly what earlier versions of this crate
//! wrote. A host whose stored events and API clients use the flat shape calls
//! [`set_wire_shape`]`(`[`WireShape::Flat`]`)` once at start-up. Reading never depends on the
//! option: both shapes always deserialise.
//!
//! | Field | `Standard` (default) | `Flat` |
//! | --- | --- | --- |
//! | `PermissionGateRequest.binding`, `PendingApprovalContext.binding` | always written | left out while empty (use `payloadDigest`) |
//! | `AgentRosterRow.provider`, `AgentRosterRow.costUsd` | left out when `None` | always written (`null` = unknown) |
//! | `ProtocolEvent::AgentRosterUpdated.totalCostUsd`, `.costKnown` | left out when `None` / `true` | always written |

use rightkit_approvals::Binding;
use std::sync::atomic::{AtomicBool, Ordering};

/// Which of the two persisted/wire shapes a host writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WireShape {
    /// The shape this crate has always written (default).
    #[default]
    Standard,
    /// The flat shape: approvals carry `payloadDigest` instead of an empty `binding`, and roster
    /// cost fields are always present.
    Flat,
}

static FLAT: AtomicBool = AtomicBool::new(false);

/// Sets the wire shape for the whole process. Call once at start-up, before any value is
/// serialised.
pub fn set_wire_shape(shape: WireShape) {
    FLAT.store(shape == WireShape::Flat, Ordering::Relaxed);
}

/// The wire shape in force.
pub fn wire_shape() -> WireShape {
    if FLAT.load(Ordering::Relaxed) {
        WireShape::Flat
    } else {
        WireShape::Standard
    }
}

fn flat() -> bool {
    FLAT.load(Ordering::Relaxed)
}

pub(crate) fn omit_binding(binding: &Binding) -> bool {
    flat() && *binding == Binding::default()
}

pub(crate) fn omit_none<T>(value: &Option<T>) -> bool {
    value.is_none() && !flat()
}

pub(crate) fn omit_true(value: &bool) -> bool {
    *value && !flat()
}
