//! The private IPC service that carries the owner's licence snapshot (rightkit#1501).
//!
//! The loopback HTTP route `/internal/license-snapshot` only had the daemon's per-launch bearer
//! token: it could not say which process held it. The snapshot now travels over `rightkit-daemon`'s
//! current-user-only local transport, which identifies its peer:
//!
//! * Service name `engine-license-<port>` (`<port>` is this daemon's loopback port, so every
//!   daemon instance has its own endpoint and none takes over another's). The owner finds the port
//!   in the daemon record it already reads, connects with
//!   `Client::connect_expecting(<this daemon's executable>)`, says `hello` as app `{app}`
//!   ([`crate::license::expected_app`]) and calls [`LICENSE_SNAPSHOT_METHOD`].
//! * Params `{"frame": "<base64>"}`: one `rightkit_credentials::handoff` Reply frame
//!   (`write_message(&Message::reply_snapshot(id, client.engine_snapshot()))`), standard base64.
//!   Result `{"ack": "<base64>"}`: the Ack frame, to check with `expect_ack(read_message(..), id)`.
//! * Errors: `forbidden` (the peer is not the owner: wrong user, executable or signature; nothing
//!   was applied), `invalid_params` (not exactly one Reply frame; nothing changed), `internal`.
//! * Before the frame is applied, [`LicenseGate::deliver_from_owner`] runs the owner check
//!   ([`PeerPolicy::host_expectation`](rightkit_daemon::PeerPolicy::host_expectation) over the
//!   connection's kernel-recorded peer). The `hello` app label is only a claim and the allowlist
//!   only narrows who may ask; the peer check is what authorises.
//!
//! Without a complete owner locator ([`OwnerLocator`](crate::license::OwnerLocator)) no service is
//! hosted, no delivery is possible and the daemon stays Free: it never accepts an unsigned owner.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rightkit_daemon::{
    HostOptions, MethodDescriptor, Registration, Service, ServiceError, ServiceIdentity, Suite,
    ERROR_FORBIDDEN, LICENSE_SNAPSHOT_METHOD,
};
use serde_json::{json, Value};

use crate::license::{expected_app, launch_locators, DeliveryError, LicenseGate, LicenseLocators};

/// The rank the licence service publishes. It is unique per daemon, so no takeover contest
/// between daemons ever happens; the value only has to be valid.
const SERVICE_RANK: u8 = 100;
/// The largest base64 `frame` accepted, in characters. A snapshot is a few kilobytes; the daemon's
/// 16 MiB frame limit is far above what a licence needs.
const MAX_FRAME_B64: usize = 256 * 1024;

/// The service name of the daemon listening on loopback `port`.
pub fn service_name(port: u16) -> String {
    format!("engine-license-{port}")
}

/// Host the licence snapshot service for this daemon over the launch locators
/// ([`launch_locators`]), or `Ok(None)` when the launch gave no complete owner identity. See
/// [`host_license_service`].
pub fn host_launch_license_service(
    gate: Arc<LicenseGate>,
    port: u16,
) -> Result<Option<Service>, String> {
    match launch_locators() {
        Some(locators) => host_license_service(gate, locators, port),
        None => Ok(None),
    }
}

/// Host the licence snapshot service for this daemon, or `Ok(None)` when the launch gave no complete
/// owner identity (the daemon then accepts no delivery and stays Free). The returned [`Service`]
/// keeps the endpoint `engine-license-<port>` published until it is dropped: hold it for the
/// daemon's lifetime.
#[expect(
    clippy::result_large_err,
    reason = "Registration::method fixes the handler's error type as rightkit_daemon::ServiceError, which is 128 bytes in this build; the closure cannot box it (bogusyogi/rightkit#1854)"
)]
pub fn host_license_service(
    gate: Arc<LicenseGate>,
    locators: &LicenseLocators,
    port: u16,
) -> Result<Option<Service>, String> {
    let Some(owner) = locators.owner.as_ref() else {
        return Ok(None);
    };
    let Some(policy) = owner.policy() else {
        return Ok(None);
    };
    let identity = ServiceIdentity::new(
        &service_name(port),
        "engine-license",
        SERVICE_RANK,
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())?;
    let registration = Registration::new(identity)
        .method(
            MethodDescriptor::local(LICENSE_SNAPSHOT_METHOD, [expected_app()]),
            move |cx, params, _emit| {
                let frame = decode_frame(params)
                    .map_err(|message| ServiceError::new("invalid_params", message))?;
                let ack = gate
                    .deliver_from_owner(cx, &policy.host_expectation(), &frame)
                    .map_err(delivery_error)?;
                Ok(json!({ "ack": STANDARD.encode(ack) }))
            },
        )
        .map_err(|error| error.to_string())?;
    let suite = Suite::resolve().map_err(|error| error.to_string())?;
    let service = Service::builder()
        .register(&suite, registration, HostOptions::new())
        .map_err(|error| error.to_string())?
        .start()
        .map_err(|error| error.to_string())?;
    Ok(Some(service))
}

/// The `frame` parameter, base64-decoded, within [`MAX_FRAME_B64`].
fn decode_frame(params: &Value) -> Result<Vec<u8>, &'static str> {
    let encoded = params
        .get("frame")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= MAX_FRAME_B64)
        .ok_or("frame is required")?;
    STANDARD.decode(encoded).map_err(|_| "frame is not base64")
}

/// The wire error for a refused delivery. The message names no process and no path.
fn delivery_error(error: DeliveryError) -> ServiceError {
    match error {
        DeliveryError::Unauthenticated => ServiceError::new(ERROR_FORBIDDEN, "owner only"),
        DeliveryError::Malformed => ServiceError::new("invalid_params", "not one reply frame"),
        DeliveryError::Ack => ServiceError::new("internal", "the acknowledgement was not encoded"),
    }
}
