//! The target side of a relayed phone request: the strict parameter checks and the key the target keeps
//! its act under. The host verifies the phone's signature itself (`rightkit_owner_approvals::admin_sig`,
//! `check_relay`) and builds the task; nothing here accepts a request on its word.
//!
//! Failures are `(code, message)` pairs.

use serde_json::Value;

/// Longest request, in bytes (what `agent.act` accepts).
pub const REQUEST_MAX: usize = 4096;

/// The extra characters a plain id may hold beside letters and digits.
pub const ID_EXTRA: [char; 7] = ['.', '_', ':', '@', '#', '=', '-'];

/// The extra characters a client message id may hold beside letters and digits.
pub const CLIENT_EXTRA: [char; 2] = ['-', '_'];

/// A refusal code and message.
pub type Failure = (String, String);

/// A refusal.
pub fn fail(code: &str, message: &str) -> Failure {
    (code.to_string(), message.to_string())
}

/// A plain value: 1 to `max` bytes of letters, digits and the characters of `extra`.
pub fn plain(params: &Value, key: &str, max: usize, extra: &[char]) -> Result<String, Failure> {
    match params.get(key).and_then(Value::as_str) {
        Some(text)
            if !text.is_empty()
                && text.len() <= max
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c)) =>
        {
            Ok(text.to_string())
        }
        _ => Err(fail(
            "invalid_params",
            &format!("param `{key}` is missing or not a plain id"),
        )),
    }
}

/// The phone id param (`phone`) or task id param, as a plain id.
pub fn id_value(params: &Value, key: &str) -> Result<String, Failure> {
    plain(params, key, 128, &ID_EXTRA)
}

/// The client message id param (`client`).
pub fn client_value(params: &Value) -> Result<String, Failure> {
    plain(params, "client", 64, &CLIENT_EXTRA)
}

/// The `request` param, trimmed, 1 to [`REQUEST_MAX`] bytes.
pub fn request_value(params: &Value) -> Result<&str, Failure> {
    params
        .get("request")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= REQUEST_MAX)
        .ok_or_else(|| fail("invalid_params", "`request` is missing or too long"))
}

/// The idempotency key of the act this computer starts for one relayed request: it names the phone and its
/// client message.
pub fn key_of(phone: &str, client: &str) -> String {
    format!("phone-relay:{phone}:{client}")
}

/// Whether a task of the acting kind (`kind_is_act`) with idempotency key `key` is the act this computer
/// started for that phone and client: the paired server follows and cancels only that one.
pub fn is_relayed_act(kind_is_act: bool, key: Option<&str>, phone: &str, client: &str) -> bool {
    kind_is_act && key == Some(key_of(phone, client).as_str())
}
