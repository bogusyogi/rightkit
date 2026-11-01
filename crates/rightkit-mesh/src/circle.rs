//! The owner circle (D4): introductions and revoke-only third-party revocation.
//!
//! Pairing a new device with ANY member makes it a member of the owner's circle. That member
//! sends every other member an [`Introduction`], and sends the new device one for each member, so
//! nobody pairs N(N-1)/2 times. Rules, from Adrian's D4 answer (2026-10-17):
//!
//! * An introduction adds the device everywhere with NO grants. Grants stay a local, per-device
//!   choice made in the hub.
//! * A member may revoke a third device. Revocation only removes trust; the worst a stolen
//!   member can do is revoke the others, which forces a re-pair. It can never add trust or a
//!   grant.
//! * Messages travel over a live stream or through the sealed mailbox, so the sender is the
//!   paired device the channel authenticated. A message is refused when it is older than the
//!   mailbox TTL or issued in the future beyond clock skew.
//!
//! This module holds the messages, their checks and the fan-out plan. The registry change itself
//! is the [`PeerTrust`] implementation's, so the mesh keeps no second device list.

use serde::{Deserialize, Serialize};

use crate::error::MeshError;
use crate::identity::{IntroOutcome, PeerInfo, PeerTrust};
use crate::util::unhex;
use rightkit_fs::digest::hex;

/// A circle message older than this is ignored (the mailbox TTL).
pub const CIRCLE_MESSAGE_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// A circle message issued further in the future than this is refused.
pub const CIRCLE_CLOCK_SKEW_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    /// The introduced device's public key, 64 hex characters.
    pub endpoint: String,
    /// Its device-link id. The [`PeerTrust`] implementation checks that it derives from `endpoint`.
    pub device_id: String,
    pub platform: String,
    pub display_name: Option<String>,
    /// The device id of the member introducing it; must be the sender of this message.
    pub introduced_by: String,
    /// The pairing during which the circle grew, for the "added by" notice.
    pub pairing_id: String,
    pub issued_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CircleMessage {
    Introduction(Introduction),
    /// "Stop trusting `target_device_id`". Removes trust only.
    Revoke {
        target_device_id: String,
        issued_ms: u64,
    },
}

/// What a circle message did on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CircleOutcome {
    Introduced(IntroOutcome),
    Revoked { target_device_id: String },
}

fn check_age(issued_ms: u64, now_ms: u64) -> Result<(), MeshError> {
    if issued_ms.saturating_add(CIRCLE_MESSAGE_MAX_AGE_MS) < now_ms {
        return Err(MeshError::Malformed("circle message expired"));
    }
    if issued_ms > now_ms.saturating_add(CIRCLE_CLOCK_SKEW_MS) {
        return Err(MeshError::Malformed("circle message from the future"));
    }
    Ok(())
}

/// Apply a circle message that arrived from the device with key `from_endpoint`.
pub fn handle_circle_message(
    trust: &dyn PeerTrust,
    from_endpoint: &[u8; 32],
    message: &CircleMessage,
    now_ms: u64,
) -> Result<CircleOutcome, MeshError> {
    // A cloud agent (Pulse #492) adds and removes no devices.
    let from = trust
        .peer(from_endpoint)
        .filter(|peer| !peer.guest)
        .ok_or(MeshError::NotPaired)?;
    match message {
        CircleMessage::Introduction(intro) => {
            check_age(intro.issued_ms, now_ms)?;
            if intro.introduced_by != from.device_id {
                return Err(MeshError::Malformed("introduction not from its introducer"));
            }
            unhex::<32>(&intro.endpoint)?;
            let outcome = trust.apply_introduction(&from, intro)?;
            Ok(CircleOutcome::Introduced(outcome))
        }
        CircleMessage::Revoke {
            target_device_id,
            issued_ms,
        } => {
            check_age(*issued_ms, now_ms)?;
            trust.apply_revoke(&from, target_device_id, now_ms)?;
            Ok(CircleOutcome::Revoked {
                target_device_id: target_device_id.clone(),
            })
        }
    }
}

/// The messages to send after this device paired with `new`: each existing member learns of `new`,
/// and `new` learns of each existing member. Returns `(recipient endpoint, message)` pairs for the
/// caller to deliver live or through the mailbox.
pub fn plan_introductions(
    local_device_id: &str,
    new: &PeerInfo,
    others: &[PeerInfo],
    pairing_id: &str,
    now_ms: u64,
) -> Vec<([u8; 32], CircleMessage)> {
    let intro = |about: &PeerInfo| {
        CircleMessage::Introduction(Introduction {
            endpoint: hex(&about.endpoint),
            device_id: about.device_id.clone(),
            platform: about.platform.clone(),
            display_name: about.display_name.clone(),
            introduced_by: local_device_id.to_string(),
            pairing_id: pairing_id.to_string(),
            issued_ms: now_ms,
        })
    };
    let mut plan = Vec::new();
    for member in others.iter().filter(|m| m.endpoint != new.endpoint) {
        plan.push((member.endpoint, intro(new)));
        plan.push((new.endpoint, intro(member)));
    }
    plan
}

/// Revoke messages to send every other member after this device revoked `target_device_id`.
pub fn plan_revokes(
    target_device_id: &str,
    others: &[PeerInfo],
    now_ms: u64,
) -> Vec<([u8; 32], CircleMessage)> {
    others
        .iter()
        .filter(|m| m.device_id != target_device_id)
        .map(|m| {
            (
                m.endpoint,
                CircleMessage::Revoke {
                    target_device_id: target_device_id.to_string(),
                    issued_ms: now_ms,
                },
            )
        })
        .collect()
}
