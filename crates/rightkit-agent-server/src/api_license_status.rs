//! `GET /license/status`: a read of the licence gate and nothing else.
//!
//! No route accepts a licence snapshot: the owner delivers it over the daemon's local
//! transport (see `license.rs`). This route only reports what the gate holds, so a journey or
//! the installed-app check can observe it. The answer carries no secret: never a token, a
//! feature list, a device id, a frame or an owner path.
//!
//! Authentication is the daemon's ordinary bearer middleware (`auth.rs`): only the owner token is
//! admitted. A missing or wrong bearer is 401 and a guest link is 403, because
//! `guest_shares::guest_allows` denies every path except `/guest/whoami` and the guest's own
//! `/sessions/{id}/` routes. This path is on no exemption list. The route is mounted only on a
//! host that turns `host_options::license_status_route` on.

use axum::{extract::State, http::StatusCode, response::Json, routing::get, Router};
use serde_json::{json, Value};

use crate::api::DaemonState;

pub(crate) fn license_status_routes() -> Router<DaemonState> {
    Router::new().route("/license/status", get(get_status))
}

async fn get_status(State(state): State<DaemonState>) -> (StatusCode, Json<Value>) {
    let (tier, delivered, deliveries) = match state.license_gate.as_ref() {
        Some(gate) => (gate.tier_label(), gate.delivered(), gate.deliveries()),
        None => ("free", false, 0),
    };
    (
        StatusCode::OK,
        Json(json!({
            "tier": tier,
            "delivered": delivered,
            "deliveries": deliveries,
            "enforced": state.license_enforced,
        })),
    )
}
