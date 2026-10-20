//! The stored licence record as text, read without any client machinery.
//!
//! The M12 native client stores the licence as an envelope in one credential item:
//! `{"rkv":1,"revision":"<random 128-bit>","state":"active"|"tombstone","record":{...}}`. A
//! tombstone is a deactivated licence and means no licence at all. Before M12 an app stored the
//! bare `@rightkit/license` record (`{"status":"active","signedToken":"...",...}`); a bare record
//! has no `rkv` key and is read as active with no revision. This module reads either form and is
//! always compiled, so an engine that keeps its own reader closure (see
//! [`crate::CredentialTokenSource`]) understands both without enabling the `client` feature.
//!
//! It extracts the signed token only. The token is still authenticated by the verifier after it is
//! loaded; nothing here grants anything.

use serde_json::{Map, Value};

/// The top-level key that marks an envelope.
pub(crate) const ENVELOPE_KEY: &str = "rkv";
/// The envelope version this crate writes and reads.
pub(crate) const ENVELOPE_VERSION: u64 = 1;

const UNREADABLE: &str = "stored licence record is unreadable";

/// A record whose status is one of these holds no licence, whatever token it still carries.
fn status_denies(status: &str) -> bool {
    matches!(
        status,
        "invalid" | "revoked" | "removed" | "failed" | "none"
    )
}

/// The signed token in the stored record text `text` (an envelope or a bare record).
///
/// - `Ok(None)`: no licence. A tombstone, a record whose status is not active, or an active
///   record that holds no signed token.
/// - `Ok(Some(token))`: the stored signed token, unverified.
/// - `Err(_)`: the text is not a readable record. Callers fail closed; this is never "no licence".
///
/// The error text is fixed and never quotes the input.
pub fn stored_signed_token(text: &str) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| UNREADABLE.to_string())?;
    let Some(top) = value.as_object() else {
        return Err(UNREADABLE.to_string());
    };
    let record: &Map<String, Value> = if top.contains_key(ENVELOPE_KEY) {
        if top.get(ENVELOPE_KEY).and_then(Value::as_u64) != Some(ENVELOPE_VERSION) {
            return Err("stored licence record has an unsupported version".to_string());
        }
        match top.get("state").and_then(Value::as_str) {
            Some("tombstone") => return Ok(None),
            Some("active") => {}
            _ => return Err(UNREADABLE.to_string()),
        }
        match top.get("record").and_then(Value::as_object) {
            Some(record) => record,
            None => return Err(UNREADABLE.to_string()),
        }
    } else {
        top
    };
    if record
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(status_denies)
    {
        return Ok(None);
    }
    Ok(record
        .get("signedToken")
        .or_else(|| record.get("signed_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_record_is_active_with_no_revision() {
        let text = r#"{"status":"active","deviceId":"d","signedToken":"a.b"}"#;
        assert_eq!(stored_signed_token(text), Ok(Some("a.b".to_string())));
        let snake = r#"{"signed_token":"c.d"}"#;
        assert_eq!(stored_signed_token(snake), Ok(Some("c.d".to_string())));
    }

    #[test]
    fn a_bare_record_that_is_not_active_or_holds_no_token_is_no_licence() {
        for status in ["invalid", "revoked", "removed", "failed", "none"] {
            let text = format!(r#"{{"status":"{status}","signedToken":"a.b"}}"#);
            assert_eq!(stored_signed_token(&text), Ok(None), "{status}");
        }
        assert_eq!(stored_signed_token(r#"{"status":"active"}"#), Ok(None));
        assert_eq!(
            stored_signed_token(r#"{"status":"active","signedToken":"  "}"#),
            Ok(None)
        );
    }

    #[test]
    fn an_active_envelope_yields_its_token_and_a_tombstone_yields_none() {
        let active = r#"{"rkv":1,"revision":"r","state":"active","record":{"status":"active","signedToken":"a.b"}}"#;
        assert_eq!(stored_signed_token(active), Ok(Some("a.b".to_string())));
        // A tombstone never yields a token, even one carrying a stale record.
        let tomb = r#"{"rkv":1,"revision":"r","state":"tombstone","record":{"signedToken":"a.b"}}"#;
        assert_eq!(stored_signed_token(tomb), Ok(None));
    }

    #[test]
    fn unreadable_or_unknown_envelopes_fail_closed_with_fixed_text() {
        for text in [
            "",
            "not json",
            "[]",
            r#"{"rkv":2,"state":"active","record":{}}"#,
            r#"{"rkv":1,"state":"weird","record":{}}"#,
            r#"{"rkv":1,"state":"active"}"#,
        ] {
            let error = stored_signed_token(text).unwrap_err();
            assert!(
                error.starts_with("stored licence record"),
                "{text}: {error}"
            );
        }
    }
}
