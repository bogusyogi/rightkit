//! The HTTPS calls of the native client.
//!
//! Same wire as `@rightkit/license` 0.2.1: `POST https://{host}/v1/licenses/{activate|refresh|
//! deactivate|trial}` with JSON bodies, and the device-lease routes
//! `POST https://{host}/v1/pro/{app}/devices/lease[/renew|/remove]`, the Pro-bridge routes
//! `POST /v1/licenses/bridge/{from-activation|exchange}` and the link routes
//! `POST /v1/pro/{app}/devices/windows-link/{request|redeem}` (the shapes HeardRight and
//! ScrapeRight send today). The host is compiled in
//! (the config takes no URL), the HTTP client is first-party (`https_only`, exact host, public
//! addresses, no redirects) and bodies, tokens and keys are registered as secrets so the HTTP
//! layer scrubs them from its errors. Server text is read only to classify (`status`, `ok`); it
//! is never forwarded.

use rightkit_http::{Client, Request};
use serde_json::{json, Value};
use zeroize::{Zeroize, Zeroizing};

use super::types::LicenseClientConfig;
use super::view::LicenseError;
use crate::{RefreshRequest, RefreshResponse, RefreshTransport, TransportError};

/// A response: the status and the body, which can hold tokens and so is zeroed on drop.
pub(crate) struct Reply {
    pub status: u16,
    pub body: Zeroizing<Vec<u8>>,
}

/// How the server answered an activate or refresh call.
pub(crate) enum Answer {
    /// A parsed body with a string `status` (2xx, or an authoritative 4xx with `ok:false`).
    Body(RefreshResponse),
    /// Anything else: an unparsable body, a 5xx, a 4xx without `ok:false`. Not authoritative.
    Unusable { http: u16 },
}

/// The `{ok}` of a trial answer.
pub(crate) enum TrialAnswer {
    Accepted,
    Declined,
    RateLimited,
}

pub(crate) struct Transport {
    client: Client,
    origin: String,
    app: String,
    app_version: String,
}

impl Transport {
    pub(crate) fn new(config: &LicenseClientConfig) -> Result<Self, LicenseError> {
        let client = Client::try_new(config.http.clone()).map_err(|_| LicenseError::Network)?;
        Ok(Self {
            client,
            origin: config.origin(),
            app: config.app.clone(),
            app_version: config.app_version.clone(),
        })
    }

    fn post(&self, path: &str, body: &Value, secrets: &[&str]) -> Result<Reply, LicenseError> {
        let mut bytes =
            Zeroizing::new(serde_json::to_vec(body).map_err(|_| LicenseError::Network)?);
        let mut request = Request::post(format!("{}{path}", self.origin))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(bytes.to_vec());
        bytes.zeroize();
        for secret in secrets {
            request = request.redact_value(*secret);
        }
        let result = self.client.send_any(&request);
        request.body.zeroize();
        match result {
            Ok(response) => Ok(Reply {
                status: response.status,
                body: Zeroizing::new(response.body),
            }),
            // A transport failure, a timeout, a cancel or a policy refusal: no licence answer
            // either way. The HTTP error text is dropped (it is scrubbed, but not needed).
            Err(_) => Err(LicenseError::Network),
        }
    }

    /// `POST /v1/licenses/activate`.
    pub(crate) fn activate(&self, device_id: &str, key: &str) -> Result<Answer, LicenseError> {
        let body = json!({
            "app": self.app,
            "deviceId": device_id,
            "licenseKey": key,
            "version": self.app_version,
        });
        let reply = self.post("/v1/licenses/activate", &body, &[key])?;
        Ok(license_answer(&reply))
    }

    /// `POST /v1/licenses/refresh`.
    pub(crate) fn refresh(
        &self,
        activation_token: &str,
        device_id: &str,
    ) -> Result<Answer, LicenseError> {
        let body = json!({
            "app": self.app,
            "activationToken": activation_token,
            "deviceId": device_id,
        });
        let reply = self.post("/v1/licenses/refresh", &body, &[activation_token])?;
        Ok(license_answer(&reply))
    }

    /// `POST /v1/licenses/deactivate`, best effort: the local clear is authoritative.
    pub(crate) fn deactivate(&self, activation_token: &str) -> bool {
        let body = json!({ "app": self.app, "activationToken": activation_token });
        matches!(
            self.post("/v1/licenses/deactivate", &body, &[activation_token]),
            Ok(reply) if (200..300).contains(&reply.status)
        )
    }

    /// `POST /v1/licenses/trial`. Issues no credential; the key arrives by e-mail.
    pub(crate) fn trial(&self, email: &str) -> Result<TrialAnswer, LicenseError> {
        let body = json!({ "app": self.app, "email": email });
        let reply = self.post("/v1/licenses/trial", &body, &[])?;
        if reply.status == 429 {
            return Ok(TrialAnswer::RateLimited);
        }
        if !(200..300).contains(&reply.status) {
            return Ok(TrialAnswer::Declined);
        }
        let accepted = serde_json::from_slice::<Value>(&reply.body)
            .ok()
            .and_then(|value| value.get("ok").and_then(Value::as_bool))
            == Some(true);
        Ok(if accepted {
            TrialAnswer::Accepted
        } else {
            TrialAnswer::Declined
        })
    }

    /// `POST /v1/pro/{app}/devices/lease` (issue).
    pub(crate) fn lease_issue(
        &self,
        key: &str,
        device_id_hash: &str,
        platform: &str,
        label: Option<&str>,
    ) -> Result<Reply, LicenseError> {
        let mut body = json!({
            "licenseKey": key,
            "deviceIdHash": device_id_hash,
            "platform": platform,
        });
        if let Some(label) = label {
            body["deviceLabel"] = json!(label);
        }
        self.post(
            &format!("/v1/pro/{}/devices/lease", self.app),
            &body,
            &[key],
        )
    }

    /// `POST /v1/pro/{app}/devices/lease/renew`.
    pub(crate) fn lease_renew(
        &self,
        device_id_hash: &str,
        lease_id: &str,
    ) -> Result<Reply, LicenseError> {
        let body = json!({ "deviceIdHash": device_id_hash, "leaseId": lease_id });
        self.post(
            &format!("/v1/pro/{}/devices/lease/renew", self.app),
            &body,
            &[],
        )
    }

    /// `POST /v1/licenses/bridge/from-activation`: mint a Pro-bridge token from this device's
    /// activation. Body as the apps send it today: `{app, activationToken, deviceId, version}`.
    pub(crate) fn bridge_from_activation(
        &self,
        activation_token: &str,
        device_id: &str,
    ) -> Result<Reply, LicenseError> {
        let body = json!({
            "app": self.app,
            "activationToken": activation_token,
            "deviceId": device_id,
            "version": self.app_version,
        });
        self.post(
            "/v1/licenses/bridge/from-activation",
            &body,
            &[activation_token],
        )
    }

    /// `POST /v1/licenses/bridge/exchange`: a bridge token for a device lease on this device.
    /// Body as the apps send it today: `{app, bridgeToken, deviceId, version, platform,
    /// deviceClass}`.
    pub(crate) fn bridge_exchange(
        &self,
        bridge_token: &str,
        device_id: &str,
        platform: &str,
        device_class: &str,
    ) -> Result<Reply, LicenseError> {
        let body = json!({
            "app": self.app,
            "bridgeToken": bridge_token,
            "deviceId": device_id,
            "version": self.app_version,
            "platform": platform,
            "deviceClass": device_class,
        });
        self.post("/v1/licenses/bridge/exchange", &body, &[bridge_token])
    }

    /// `POST /v1/pro/{app}/devices/windows-link/request`: a link code, proven by this device's
    /// activation and lease. Body `{activationToken, leaseId, deviceIdHash}`.
    pub(crate) fn link_request(
        &self,
        activation_token: &str,
        lease_id: &str,
        device_id_hash: &str,
    ) -> Result<Reply, LicenseError> {
        let body = json!({
            "activationToken": activation_token,
            "leaseId": lease_id,
            "deviceIdHash": device_id_hash,
        });
        self.post(
            &format!("/v1/pro/{}/devices/windows-link/request", self.app),
            &body,
            &[activation_token],
        )
    }

    /// `POST /v1/pro/{app}/devices/windows-link/redeem`: a lease for this device from a link
    /// code. Body `{code, deviceIdHash, deviceLabel?}`.
    pub(crate) fn link_redeem(
        &self,
        code: &str,
        device_id_hash: &str,
        label: Option<&str>,
    ) -> Result<Reply, LicenseError> {
        let mut body = json!({ "code": code, "deviceIdHash": device_id_hash });
        if let Some(label) = label {
            body["deviceLabel"] = json!(label);
        }
        self.post(
            &format!("/v1/pro/{}/devices/windows-link/redeem", self.app),
            &body,
            &[code],
        )
    }

    /// `POST /v1/pro/{app}/devices/lease/remove`, best effort.
    pub(crate) fn lease_remove(&self, device_id_hash: &str, lease_id: &str) -> bool {
        let body = json!({ "deviceIdHash": device_id_hash, "leaseId": lease_id });
        matches!(
            self.post(
                &format!("/v1/pro/{}/devices/lease/remove", self.app),
                &body,
                &[],
            ),
            Ok(reply) if (200..300).contains(&reply.status)
        )
    }
}

/// Classify an activate or refresh answer exactly as `@rightkit/license` 0.2.1 does: only an
/// explicit client-side rejection (4xx, `ok:false`, a string `status` other than `active`) is
/// authoritative; a proxy error, a malformed body or a 5xx is not.
fn license_answer(reply: &Reply) -> Answer {
    let parsed = serde_json::from_slice::<RefreshResponse>(&reply.body).ok();
    if (200..300).contains(&reply.status) {
        return match parsed {
            Some(body) => Answer::Body(body),
            None => Answer::Unusable { http: reply.status },
        };
    }
    if (400..500).contains(&reply.status) {
        let ok_false = serde_json::from_slice::<Value>(&reply.body)
            .ok()
            .and_then(|value| value.get("ok").and_then(Value::as_bool))
            == Some(false);
        if let (true, Some(body)) = (ok_false, parsed) {
            if body.status != "active" {
                return Answer::Body(body);
            }
        }
    }
    Answer::Unusable { http: reply.status }
}

/// The existing [`RefreshTransport`] contract over [`Transport`]: `Ok` for an authoritative
/// answer, `Err` for everything else (which becomes `Stale`, never `Revoked`).
pub(crate) struct HttpRefreshTransport<'a> {
    pub transport: &'a Transport,
}

impl RefreshTransport for HttpRefreshTransport<'_> {
    fn refresh(&self, request: &RefreshRequest<'_>) -> Result<RefreshResponse, TransportError> {
        match self
            .transport
            .refresh(request.activation_token, request.device_id)
        {
            Ok(Answer::Body(body)) => Ok(body),
            // Fixed text: the server's words are never carried into the outcome.
            Ok(Answer::Unusable { .. }) => Err(TransportError::new("refresh answer unusable")),
            Err(_) => Err(TransportError::new("refresh unavailable")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: Zeroizing::new(body.as_bytes().to_vec()),
        }
    }

    fn status_of(answer: &Answer) -> Option<&str> {
        match answer {
            Answer::Body(body) => Some(body.status.as_str()),
            Answer::Unusable { .. } => None,
        }
    }

    #[test]
    fn a_2xx_body_with_a_status_is_an_answer() {
        let answer = license_answer(&reply(
            200,
            r#"{"ok":true,"status":"active","signedToken":"a.b"}"#,
        ));
        assert_eq!(status_of(&answer), Some("active"));
        let answer = license_answer(&reply(200, r#"{"status":"revoked"}"#));
        assert_eq!(status_of(&answer), Some("revoked"));
    }

    #[test]
    fn only_an_explicit_4xx_rejection_is_authoritative() {
        let rejected = license_answer(&reply(
            400,
            r#"{"ok":false,"status":"invalid","message":"x"}"#,
        ));
        assert_eq!(status_of(&rejected), Some("invalid"));
        // ok:false is required.
        let no_flag = license_answer(&reply(400, r#"{"status":"invalid"}"#));
        assert!(matches!(no_flag, Answer::Unusable { http: 400 }));
        // A 4xx that says active is not a rejection.
        let active = license_answer(&reply(400, r#"{"ok":false,"status":"active"}"#));
        assert!(matches!(active, Answer::Unusable { .. }));
        // 5xx, proxy HTML and malformed bodies are not authoritative.
        for (status, body) in [
            (500, r#"{"ok":false,"status":"invalid"}"#),
            (502, "<html>"),
            (200, "nope"),
        ] {
            assert!(
                matches!(
                    license_answer(&reply(status, body)),
                    Answer::Unusable { .. }
                ),
                "{status}"
            );
        }
        assert!(matches!(
            license_answer(&reply(429, "")),
            Answer::Unusable { http: 429 }
        ));
    }
}
