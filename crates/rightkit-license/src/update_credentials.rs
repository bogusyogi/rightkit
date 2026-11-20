//! The two facts an update-check request needs from the stored licence record, and the headers
//! built from them.
//!
//! Read-only and native only. [`stored_update_credentials`] accepts both the bare record an older
//! app stored and the revisioned envelope (`rkv`); a tombstone is no licence; unreadable text is an
//! error, never "no licence". The error text is fixed and never quotes the input. Nothing here
//! authenticates the token or grants anything: the entitlement check is
//! `NativeEngineEntitlements`. Remove this module once the app's updater adopts the
//! `update-auth` provider.

use std::collections::HashMap;

const MALFORMED: &str = "stored license is malformed";

/// What the update-check headers need from the stored licence record.
/// Contains credential material; intentionally has no Debug or serialization implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredUpdateCredentials {
    pub active: bool,
    pub activation_token: Option<String>,
}

/// Read the stored record text (bare record, or the revisioned envelope). Unlike
/// [`crate::stored_signed_token`] this does not check the envelope version number: it reads
/// exactly what the app's own reader always read.
pub fn stored_update_credentials(text: &str) -> Result<Option<StoredUpdateCredentials>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| MALFORMED.to_string())?;
    let top = value.as_object().ok_or_else(|| MALFORMED.to_string())?;
    let record = if top.contains_key("rkv") {
        match top.get("state").and_then(|state| state.as_str()) {
            Some("tombstone") => return Ok(None),
            Some("active") => top
                .get("record")
                .and_then(|record| record.as_object())
                .ok_or_else(|| MALFORMED.to_string())?,
            _ => return Err(MALFORMED.to_string()),
        }
    } else {
        top
    };
    let text_field = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| record.get(*name))
            .and_then(|field| field.as_str())
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
    };
    Ok(Some(StoredUpdateCredentials {
        active: text_field(&["status"]).as_deref() == Some("active"),
        activation_token: text_field(&["activationToken", "activation_token"]),
    }))
}

/// What an update check needs from the stored licence. Native only; never sent to a renderer.
pub struct UpdateLicense {
    pub status: String,
    pub activation_token: Option<String>,
    pub device_id: String,
}

/// Read the stored licence for an update check.
///
/// - `read_record`: the host's one credential read: `Ok(None)` only when the store authoritatively
///   reports the item absent; its error text must not carry a secret.
/// - `device_id`: the host's device identity read, called only after a record was found (never for
///   an absent or tombstoned licence).
/// - `log`: the host's `(event, detail)` sink; receives `secure_license_get_error` with the fixed
///   error text on a read or parse failure.
pub fn read_update_license(
    read_record: impl FnOnce() -> Result<Option<String>, String>,
    device_id: impl FnOnce() -> Result<String, String>,
    log: impl Fn(&str, &str),
) -> Result<Option<UpdateLicense>, String> {
    let raw = match read_record() {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(None),
        Err(error) => {
            let message = format!("could not read license from secure storage: {error}");
            log("secure_license_get_error", &message);
            return Err(message);
        }
    };
    let Some(credentials) = stored_update_credentials(&raw).inspect_err(|message| {
        log("secure_license_get_error", message);
    })?
    else {
        return Ok(None);
    };
    Ok(Some(UpdateLicense {
        status: if credentials.active {
            "active".to_string()
        } else {
            "inactive".to_string()
        },
        activation_token: credentials.activation_token,
        device_id: device_id()?,
    }))
}

/// The host's header names for the update check (its brand header prefix).
#[derive(Debug, Clone, Copy)]
pub struct UpdateCheckHeaderNames<'a> {
    pub app_header: &'a str,
    pub app_value: &'a str,
    pub device_header: &'a str,
}

/// `Authorization: Bearer <token>`, alone. Used for paid artifact installs.
pub fn bearer_headers(token: &str) -> HashMap<String, String> {
    HashMap::from([("Authorization".to_string(), format!("Bearer {token}"))])
}

/// Headers for the update CHECK: only an active licence with an activation token yields any.
pub fn update_check_headers(
    license: UpdateLicense,
    names: UpdateCheckHeaderNames<'_>,
) -> Option<HashMap<String, String>> {
    if license.status != "active" {
        return None;
    }
    let token = license.activation_token?;
    let mut headers = bearer_headers(&token);
    headers.insert(names.app_header.to_string(), names.app_value.to_string());
    headers.insert(names.device_header.to_string(), license.device_id);
    Some(headers)
}

/// The signed entitlement token for native consumers that present it to a private artifact lane,
/// or `None` unless `pro_granted` (the host's verified Pro verdict, evaluated by the caller) and
/// the source holds a signed credential right now. Never sent to a renderer by this function.
pub fn verified_signed_token<S: crate::NativeEntitlementSource>(
    pro_granted: bool,
    source: Option<&S>,
) -> Option<String> {
    if !pro_granted {
        return None;
    }
    match source?.read() {
        Ok(crate::NativeCredential::Signed(credential)) => Some(credential.token),
        _ => None,
    }
}
