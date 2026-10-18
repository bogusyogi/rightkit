//! `POST /v1/call/{method}`: the device-facing form of the one request contract.
//!
//! A signed device (same `RK-Device` credential, `X-RK-*` signature, nonce and replay window as
//! every other device route) names a method of the service's [`MethodTable`]; the body is the
//! method's JSON params. The method must be reachable on both transports and the device must
//! hold one of its scopes. The same [`Handler`] that serves local callers then runs with a
//! [`Caller::Device`]. The answer is the handler's result as the body, or
//! `{"error":{"code","message","retryable"}}` with the status [`status_for_error`] gives the
//! code. Events a handler emits have no unary channel here and are dropped.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::contract::{new_context, status_for_error, Caller, Emit, Handler, MethodTable};
use crate::error::ServiceError;

/// The table and handler a [`Daemon`](crate::remote::Daemon) dispatches device calls to.
#[derive(Clone)]
pub struct DeviceCalls {
    table: Arc<MethodTable>,
    handler: Arc<dyn Handler>,
}

/// Connection ids for device calls start high so they never collide with a local connection id
/// in a handler that keys state on `CallContext::conn_id`.
static NEXT_CALL: AtomicU64 = AtomicU64::new(1 << 62);

impl DeviceCalls {
    pub fn new(table: MethodTable, handler: Arc<dyn Handler>) -> Self {
        Self {
            table: Arc::new(table),
            handler,
        }
    }

    pub fn table(&self) -> &MethodTable {
        &self.table
    }

    /// Run one call for an authenticated device; returns `(status, body)`. `Value::Null` body
    /// means an empty body.
    pub(crate) fn run(
        &self,
        device_id: &str,
        scopes: Vec<String>,
        method: &str,
        body: &[u8],
    ) -> (u16, Value) {
        let caller = Caller::device(device_id, scopes);
        if let Err(e) = self.table.authorize_device_call(&caller, method) {
            return error_body(&e);
        }
        if method == "ping" {
            return (200, json!({ "pong": true }));
        }
        let params = if body.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice::<Value>(body) {
                Ok(v) => v,
                Err(_) => {
                    return error_body(&ServiceError::new("bad_request", "body is not JSON"));
                }
            }
        };
        let cx = new_context(
            NEXT_CALL.fetch_add(1, Ordering::Relaxed),
            caller,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let emit: Emit = Arc::new(|_| {});
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.handler.call(&cx, method, &params, &emit)
        }))
        .unwrap_or_else(|_| Err(ServiceError::new("panic", "handler panicked")));
        match result {
            Ok(v) => (200, v),
            Err(e) => error_body(&e),
        }
    }
}

fn error_body(e: &ServiceError) -> (u16, Value) {
    (status_for_error(e), json!({ "error": e.to_value() }))
}
