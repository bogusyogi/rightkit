//! The Pro bridge and the device-link codes, as native typed operations.
//!
//! Both flows exist today as app code (HeardRight `pro_bridge.rs` and `license/lease.rs`,
//! ScrapeRight `pro_bridge.rs`); this module is the one shared implementation of their licence
//! half, over the same service paths and bodies (see `wire`):
//!
//! - **Pro bridge.** [`LicenseClient::bridge_publish`](super::LicenseClient::bridge_publish)
//!   (`POST /v1/licenses/bridge/from-activation`) mints the opaque bridge token from this
//!   device's activation, and
//!   [`bridge_exchange`](super::LicenseClient::bridge_exchange)
//!   (`POST /v1/licenses/bridge/exchange`) turns a token into a signed device lease on another
//!   device. Where the token is kept (the synchronizable store of an Apple device) stays with the
//!   app; the token itself is a [`SecretString`], native only, and unlocks nothing: the lease that
//!   comes back goes through exactly the checks of a typed-key lease (envelope signature, this
//!   device's hash, generation ordering, the nested entitlement bound to the hash, the clock) and
//!   is stored by the same fenced write.
//! - **Link codes.** [`link_request`](super::LicenseClient::link_request)
//!   (`POST /v1/pro/{app}/devices/windows-link/request`) proves ownership with this device's own
//!   activation and lease and returns a short-lived single-use code that a person types into a
//!   second device, so [`LinkCodeView`] carries it by design; it is not an entitlement and
//!   [`link_redeem`](super::LicenseClient::link_redeem)
//!   (`.../windows-link/redeem`) trades it for a lease like any other.
//!
//! Every operation holds the licence operation lock for its whole workflow (including the server
//! call), like activation, so a deactivation that begins meanwhile is `Busy` and a late answer can
//! never be written over a tombstone.

use serde::Serialize;
use serde_json::{Map, Value};

use super::envelope::{Envelope, RecordState, RecordStatus};
use super::ops::{field_str, lease_reply, lease_wire_from, wall_now, Core, LeaseWire, Locked};
use super::persist::Loaded;
use super::time::{parse_rfc3339, to_ms};
use super::types::SecretString;
use super::view::{LicenseError, LicenseView, RejectReason};
use super::wire::Reply;
use crate::MAX_TOKEN_BYTES;

/// A link code and a device label are short text; longer input is refused unsent.
const MAX_LINK_CODE_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 128;

/// The platform a Pro-bridge exchange reports, with the device class the service counts seats by.
/// The pairs are the ones the apps send today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BridgePlatform {
    /// `platform: "macos"`, `deviceClass: "mac"`.
    MacOs,
}

impl BridgePlatform {
    fn wire(self) -> (&'static str, &'static str) {
        match self {
            Self::MacOs => ("macos", "mac"),
        }
    }
}

/// A link code issued by [`LicenseClient::link_request`](super::LicenseClient::link_request),
/// for display to the person who types it into the other device. Short-lived, single-use, and not
/// an entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkCodeView {
    pub code: String,
    /// When the code stops working, Unix ms.
    pub expires_at: i64,
}

/// What a link request came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum LinkRequestOutcome {
    /// A code was issued.
    Issued(LinkCodeView),
    /// This device holds no active lease to prove ownership with (no licence, a typed-key
    /// activation with no lease, or a revoked one). Nothing was sent.
    NotLeased,
    /// The service says this licence or device is not an active lease holder (HTTP 404).
    NotActiveLeaseHolder,
}

impl Core {
    // ---- bridge: publish --------------------------------------------------------------

    pub(crate) fn bridge_publish(&self) -> Result<Option<SecretString>, LicenseError> {
        let device_id = self.device.device_id().map_err(|_| LicenseError::Verify)?;
        let Locked {
            guard: _guard,
            snap,
            _level,
        } = self.begin()?;
        // Only a licence that grants access right now can mint a bridge: an expired, revoked or
        // unverifiable record publishes nothing.
        let evaluation = self.evaluate(&snap);
        if evaluation.view.busy {
            // The clock ledger was busy: "does not grant" is not established.
            return Err(LicenseError::Busy);
        }
        let record = match (&evaluation.record, evaluation.grants) {
            (Some(record), true) if record.status == RecordStatus::Active => record,
            _ => return Ok(None),
        };
        let Some(activation) = record.activation_token().filter(|t| !t.trim().is_empty()) else {
            // No activation credential (a bare signed token): nothing a bridge can be minted from.
            return Ok(None);
        };
        let reply = self
            .transport
            .bridge_from_activation(activation, &device_id)?;
        bridge_token_of(&reply).map(Some)
    }

    // ---- bridge: exchange -------------------------------------------------------------

    pub(crate) fn bridge_exchange(
        &self,
        token: SecretString,
        platform: BridgePlatform,
    ) -> Result<LicenseView, LicenseError> {
        let trimmed = token.expose().trim();
        if trimmed.is_empty() || trimmed.len() as u64 > MAX_TOKEN_BYTES {
            // A token the service could not have issued is refused unread and unsent.
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            });
        }
        let device_id = self.device.device_id().map_err(|_| LicenseError::Verify)?;
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let evaluation = self.evaluate(&snap);
        if evaluation.view.busy {
            // The clock ledger was busy: "not licensed" is not established, so nothing is
            // replaced on the strength of it.
            return Err(LicenseError::Busy);
        }
        if evaluation.grants {
            // Already licensed: a bridge never replaces a working licence.
            self.remember(&evaluation.view);
            return Ok(evaluation.view);
        }
        let (wire_platform, device_class) = platform.wire();
        let reply =
            self.transport
                .bridge_exchange(trimmed, &device_id, wire_platform, device_class)?;
        drop(token);
        let wire = exchange_wire(&reply)?;
        let prior = match &snap.loaded {
            Loaded::Present(envelope) => envelope.record.as_ref(),
            _ => None,
        };
        let wall = wall_now()?;
        let (record, _lowered) = self.lease_record(&hash, wire, prior, wall)?;
        let envelope = Envelope::active(record)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(self.view_after(&envelope))
    }

    // ---- link codes -------------------------------------------------------------------

    pub(crate) fn link_request(&self) -> Result<LinkRequestOutcome, LicenseError> {
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let Locked {
            guard: _guard,
            snap,
            _level,
        } = self.begin()?;
        let record = match &snap.loaded {
            Loaded::Present(envelope) if envelope.state == RecordState::Active => {
                envelope.record.as_ref()
            }
            _ => None,
        };
        let Some(record) = record.filter(|r| r.status == RecordStatus::Active) else {
            return Ok(LinkRequestOutcome::NotLeased);
        };
        let (Some(activation), Some(lease)) = (
            record.activation_token().filter(|t| !t.trim().is_empty()),
            record.lease.as_ref(),
        ) else {
            return Ok(LinkRequestOutcome::NotLeased);
        };
        let reply = self
            .transport
            .link_request(activation, &lease.lease_id, &hash)?;
        match reply.status {
            200 => {}
            404 => return Ok(LinkRequestOutcome::NotActiveLeaseHolder),
            429 => {
                return Err(LicenseError::Rejected {
                    reason: RejectReason::RateLimited,
                })
            }
            _ => return Err(LicenseError::Network),
        }
        let value: Value =
            serde_json::from_slice(&reply.body).map_err(|_| LicenseError::Network)?;
        let top = value.as_object().ok_or(LicenseError::Network)?;
        let code = field_str(top, "code").ok_or(LicenseError::Verify)?;
        if code.len() > MAX_LINK_CODE_BYTES {
            return Err(LicenseError::Verify);
        }
        let expires_at = field_str(top, "expiresAt")
            .as_deref()
            .and_then(parse_rfc3339)
            .map(to_ms)
            .ok_or(LicenseError::Verify)?;
        Ok(LinkRequestOutcome::Issued(LinkCodeView {
            code,
            expires_at,
        }))
    }

    pub(crate) fn link_redeem(
        &self,
        code: &str,
        label: Option<&str>,
    ) -> Result<LicenseView, LicenseError> {
        let code = code.trim();
        if !plausible_link_code(code) {
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            });
        }
        let label = match label.map(str::trim) {
            None | Some("") => None,
            Some(label) if plausible_label(label) => Some(label),
            Some(_) => {
                return Err(LicenseError::Rejected {
                    reason: RejectReason::Other,
                })
            }
        };
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let reply = self.transport.link_redeem(code, &hash, label)?;
        // The same answer shape and status mapping as a lease issue: 404 is an unknown, expired
        // or used code, 409 a full device pool.
        let wire = lease_reply(&reply)?;
        let prior = match &snap.loaded {
            Loaded::Present(envelope) => envelope.record.as_ref(),
            _ => None,
        };
        let wall = wall_now()?;
        let (record, _lowered) = self.lease_record(&hash, wire, prior, wall)?;
        let envelope = Envelope::active(record)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(self.view_after(&envelope))
    }
}

/// The server's `status` text of an answer body, when it has one. Never forwarded.
fn body_status(reply: &Reply) -> Option<String> {
    serde_json::from_slice::<Value>(&reply.body)
        .ok()
        .and_then(|value| field_str(value.as_object()?, "status"))
}

/// A 4xx answer as a typed rejection (the server's words are not carried).
fn rejection(reply: &Reply) -> LicenseError {
    LicenseError::Rejected {
        reason: RejectReason::from_server(body_status(reply).as_deref(), reply.status),
    }
}

/// The bridge token of a `from-activation` answer. HeardRight and ScrapeRight accept `ok: true`
/// or `status: "active"`, and require a non-blank token.
fn bridge_token_of(reply: &Reply) -> Result<SecretString, LicenseError> {
    match reply.status {
        200..=299 => {}
        400..=499 => return Err(rejection(reply)),
        _ => return Err(LicenseError::Network),
    }
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| LicenseError::Network)?;
    let top = value.as_object().ok_or(LicenseError::Network)?;
    let status = field_str(top, "status");
    let accepted =
        top.get("ok").and_then(Value::as_bool) == Some(true) || status.as_deref() == Some("active");
    if !accepted {
        return Err(LicenseError::Rejected {
            reason: RejectReason::from_server(status.as_deref(), 0),
        });
    }
    let token = field_str(top, "bridgeToken").ok_or(LicenseError::Verify)?;
    if token.len() as u64 > MAX_TOKEN_BYTES {
        return Err(LicenseError::Verify);
    }
    Ok(SecretString::new(token))
}

/// The lease of an `exchange` answer: `{ok?, status: "active", activationToken, lease}`. A body
/// that says anything but `active` is a rejection; the lease checks happen in `lease_record`.
fn exchange_wire(reply: &Reply) -> Result<LeaseWire, LicenseError> {
    match reply.status {
        200..=299 => {}
        400..=499 => return Err(rejection(reply)),
        _ => return Err(LicenseError::Network),
    }
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| LicenseError::Network)?;
    let top: &Map<String, Value> = value.as_object().ok_or(LicenseError::Network)?;
    let Some(status) = field_str(top, "status") else {
        return Err(LicenseError::Verify);
    };
    if top.get("ok").and_then(Value::as_bool) == Some(false) || status != "active" {
        return Err(LicenseError::Rejected {
            reason: RejectReason::from_server(Some(status.as_str()), 0),
        });
    }
    lease_wire_from(top)
}

/// A link code is short typed text: no whitespace or control characters, bounded.
fn plausible_link_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_LINK_CODE_BYTES
        && !code.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn plausible_label(label: &str) -> bool {
    label.len() <= MAX_LABEL_BYTES && !label.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: Zeroizing::new(body.as_bytes().to_vec()),
        }
    }

    #[test]
    fn a_bridge_token_needs_ok_or_active_and_a_bounded_token() {
        let token = |status, body: &str| bridge_token_of(&reply(status, body));
        assert_eq!(
            token(200, r#"{"ok":true,"bridgeToken":"BR"}"#)
                .unwrap()
                .expose(),
            "BR"
        );
        assert_eq!(
            token(200, r#"{"status":"active","bridgeToken":" BR "}"#)
                .unwrap()
                .expose(),
            "BR"
        );
        // Neither ok nor active: a typed rejection, never a token.
        assert_eq!(
            token(200, r#"{"ok":false,"status":"revoked","bridgeToken":"BR"}"#).unwrap_err(),
            LicenseError::Rejected {
                reason: RejectReason::Revoked
            }
        );
        // No token, a blank token, and an oversized one are all refused.
        assert_eq!(
            token(200, r#"{"ok":true}"#).unwrap_err(),
            LicenseError::Verify
        );
        assert_eq!(
            token(200, r#"{"ok":true,"bridgeToken":"  "}"#).unwrap_err(),
            LicenseError::Verify
        );
        let big = format!(
            r#"{{"ok":true,"bridgeToken":"{}"}}"#,
            "x".repeat(MAX_TOKEN_BYTES as usize + 1)
        );
        assert_eq!(token(200, &big).unwrap_err(), LicenseError::Verify);
        // Statuses: 429 and 4xx are rejections, 5xx and garbage are the network.
        assert_eq!(
            token(429, "").unwrap_err(),
            LicenseError::Rejected {
                reason: RejectReason::RateLimited
            }
        );
        assert!(matches!(
            token(403, r#"{"status":"invalid"}"#).unwrap_err(),
            LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted
            }
        ));
        assert_eq!(token(502, "<html>").unwrap_err(), LicenseError::Network);
        assert_eq!(token(200, "nope").unwrap_err(), LicenseError::Network);
    }

    #[test]
    fn an_exchange_answer_must_be_active_with_a_lease() {
        let wire = |status, body: &str| exchange_wire(&reply(status, body)).map(|_| ());
        assert_eq!(
            wire(200, r#"{"ok":false,"status":"active"}"#).unwrap_err(),
            LicenseError::Rejected {
                reason: RejectReason::Other
            }
        );
        assert_eq!(
            wire(200, r#"{"status":"revoked"}"#).unwrap_err(),
            LicenseError::Rejected {
                reason: RejectReason::Revoked
            }
        );
        // No status at all, or no lease, is not something to store.
        assert_eq!(wire(200, r#"{}"#).unwrap_err(), LicenseError::Verify);
        assert_eq!(
            wire(200, r#"{"status":"active","activationToken":"A"}"#).unwrap_err(),
            LicenseError::Verify
        );
        assert_eq!(wire(500, "").unwrap_err(), LicenseError::Network);
    }

    #[test]
    fn link_codes_and_labels_are_bounded_plain_text() {
        assert!(plausible_link_code("ABCD-1234"));
        let long = "x".repeat(MAX_LINK_CODE_BYTES + 1);
        for bad in ["", "AB CD", "AB\nCD", long.as_str()] {
            assert!(!plausible_link_code(bad), "{bad:?}");
        }
        assert!(plausible_label("Work Mac"));
        assert!(!plausible_label("bad\u{7}label"));
        assert!(!plausible_label(&"x".repeat(MAX_LABEL_BYTES + 1)));
    }

    #[test]
    fn a_link_request_outcome_serializes_with_its_outcome_tag() {
        let issued = serde_json::to_value(LinkRequestOutcome::Issued(LinkCodeView {
            code: "AB-12".into(),
            expires_at: 5,
        }))
        .unwrap();
        assert_eq!(
            issued,
            serde_json::json!({"outcome":"issued","code":"AB-12","expiresAt":5})
        );
        assert_eq!(
            serde_json::to_value(LinkRequestOutcome::NotLeased).unwrap(),
            serde_json::json!({"outcome":"notLeased"})
        );
    }
}
