//! Wire format and frame authentication of `rk-link/1`.
//!
//! One frame is one line: `<tag> <payload>\n`. `payload` is compact JSON holding at least `n`
//! (the frame's sequence number, 1 for the first frame of a direction, then +1 each) and `t`
//! (the frame type). `tag` is unpadded base64url:
//!
//! * machine to host (`c2s`): the device's Ed25519 signature over [`message`];
//! * host to machine (`s2c`): HMAC-SHA256 under the session key the host issued in the
//!   upgrade response, over the same [`message`].
//!
//! `message` is `rk-link/1 \n dir \n link_id \n payload`, so a tag is bound to one
//! direction, one connection (`link_id` is chosen by the host per upgrade) and exact payload
//! bytes. The receiver verifies the tag over the bytes it received (no re-serialisation) and
//! then requires `n` to be exactly the previous `n` plus one: a replayed, duplicated, reordered
//! or dropped frame ends the connection. There is no unauthenticated frame and no plaintext
//! mode: the transport is TLS only and the client trusts only the roots it was given.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::hmac;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde_json::Value;

use crate::remote::client::DeviceKey;

/// The value of the `Upgrade` header.
pub const UPGRADE_TOKEN: &str = "rk-link/1";
/// The upgrade request target.
pub const LINK_PATH: &str = "/v1/link";
/// The device scope the upgrade requires.
pub const SCOPE: &str = "link";
/// Response header carrying the connection id (hex).
pub const H_LINK_ID: &str = "x-rk-link-id";
/// Response header carrying the host-to-machine session key (64 hex digits). Sent only inside
/// the TLS connection.
pub const H_LINK_KEY: &str = "x-rk-link-key";
/// Each side sends a `ping` at least this often.
pub const HEARTBEAT_SECS: u64 = 20;
/// A side that has received nothing for this long (three heartbeats) drops the connection.
pub const DEAD_AFTER_SECS: u64 = 60;
/// Largest accepted frame line, bytes.
pub const MAX_LINE: usize = 1 << 20;

/// Why a received line was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Not `<tag> <json>`.
    Malformed,
    /// The tag does not verify for this direction, connection and payload.
    BadTag,
}

fn message(dir: &str, link_id: &str, payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(payload.len() + 48);
    m.extend_from_slice(UPGRADE_TOKEN.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(dir.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(link_id.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(payload);
    m
}

/// Split a received line into `(tag, payload)`.
pub fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (tag, payload) = line.split_once(' ')?;
    if tag.is_empty() || payload.is_empty() {
        None
    } else {
        Some((tag, payload))
    }
}

/// Device signature (the tag) for a machine-to-host payload.
pub fn sign_c2s(key: &DeviceKey, link_id: &str, payload: &str) -> String {
    URL_SAFE_NO_PAD.encode(key.sign(&message("c2s", link_id, payload.as_bytes())))
}

/// Verify a machine-to-host tag against the device's raw 32-byte public key.
pub fn verify_c2s(public_key: &[u8], link_id: &str, payload: &str, tag: &str) -> bool {
    let Ok(sig) = URL_SAFE_NO_PAD.decode(tag) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&message("c2s", link_id, payload.as_bytes()), &sig)
        .is_ok()
}

/// Session MAC (the tag) for a host-to-machine payload.
pub fn mac_s2c(key: &[u8; 32], link_id: &str, payload: &str) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    URL_SAFE_NO_PAD.encode(hmac::sign(&k, &message("s2c", link_id, payload.as_bytes())).as_ref())
}

/// Verify a host-to-machine tag (constant time).
pub fn verify_s2c(key: &[u8; 32], link_id: &str, payload: &str, tag: &str) -> bool {
    let Ok(t) = URL_SAFE_NO_PAD.decode(tag) else {
        return false;
    };
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&k, &message("s2c", link_id, payload.as_bytes()), &t).is_ok()
}

/// A complete line (with its newline) for a machine-to-host frame. `frame` must already hold `n`.
pub fn c2s_line(key: &DeviceKey, link_id: &str, frame: &Value) -> String {
    let payload = frame.to_string();
    format!("{} {}\n", sign_c2s(key, link_id, &payload), payload)
}

/// A complete line (with its newline) for a host-to-machine frame. `frame` must already hold `n`.
pub fn s2c_line(key: &[u8; 32], link_id: &str, frame: &Value) -> String {
    let payload = frame.to_string();
    format!("{} {}\n", mac_s2c(key, link_id, &payload), payload)
}

/// Check and parse a machine-to-host line.
pub fn open_c2s(public_key: &[u8], link_id: &str, line: &str) -> Result<Value, FrameError> {
    let (tag, payload) = split_line(line).ok_or(FrameError::Malformed)?;
    if !verify_c2s(public_key, link_id, payload, tag) {
        return Err(FrameError::BadTag);
    }
    serde_json::from_str(payload).map_err(|_| FrameError::Malformed)
}

/// Check and parse a host-to-machine line.
pub fn open_s2c(key: &[u8; 32], link_id: &str, line: &str) -> Result<Value, FrameError> {
    let (tag, payload) = split_line(line).ok_or(FrameError::Malformed)?;
    if !verify_s2c(key, link_id, payload, tag) {
        return Err(FrameError::BadTag);
    }
    serde_json::from_str(payload).map_err(|_| FrameError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key() -> DeviceKey {
        DeviceKey::from_seed(&[9u8; 32]).unwrap()
    }

    #[test]
    fn c2s_roundtrip_and_binding() {
        let k = key();
        let line = c2s_line(&k, "link-a", &json!({"n":1,"t":"ping"}));
        let pk = k.public_key();
        assert!(open_c2s(&pk, "link-a", &line).is_ok());
        // another connection, another key and a changed payload all fail
        assert_eq!(open_c2s(&pk, "link-b", &line), Err(FrameError::BadTag));
        let other = DeviceKey::from_seed(&[8u8; 32]).unwrap().public_key();
        assert_eq!(open_c2s(&other, "link-a", &line), Err(FrameError::BadTag));
        let tampered = line.replace("\"n\":1", "\"n\":2");
        assert_eq!(open_c2s(&pk, "link-a", &tampered), Err(FrameError::BadTag));
    }

    #[test]
    fn directions_do_not_cross() {
        let k = key();
        let sk = [3u8; 32];
        let up = c2s_line(&k, "l", &json!({"n":1,"t":"ping"}));
        let down = s2c_line(&sk, "l", &json!({"n":1,"t":"ping"}));
        assert!(open_s2c(&sk, "l", &up).is_err());
        assert!(open_c2s(&k.public_key(), "l", &down).is_err());
        assert!(open_s2c(&sk, "l", &down).is_ok());
        assert_eq!(open_s2c(&[4u8; 32], "l", &down), Err(FrameError::BadTag));
    }

    #[test]
    fn malformed_lines() {
        assert_eq!(split_line("nospace"), None);
        assert_eq!(split_line(" x"), None);
        assert_eq!(
            open_s2c(&[0u8; 32], "l", "tag {not json}"),
            Err(FrameError::BadTag)
        );
    }
}
