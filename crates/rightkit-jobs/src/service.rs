//! Serve a [`Dispatcher`] over `rightkit-service`: the same commands, now on the
//! discovery/auth convention (service.json, current-user-only pipe/socket,
//! per-app allowlist, ranking and takeover) instead of stdio.

use std::sync::Arc;

use rightkit_service::{CallContext, Emit, Handler, ServiceError};
use serde_json::Value;

use crate::contract::Dispatcher;
use crate::sched::Gate;

/// Adapter: `Dispatcher` -> `rightkit_service::Handler`. Pass the runner's
/// [`Gate`] so a takeover pauses batch work at its next step boundary.
pub struct DispatcherHandler {
    dispatcher: Arc<Dispatcher>,
    gate: Option<Arc<Gate>>,
}

impl DispatcherHandler {
    pub fn new(dispatcher: Arc<Dispatcher>) -> Self {
        Self {
            dispatcher,
            gate: None,
        }
    }
    pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
        self.gate = Some(gate);
        self
    }
}

impl Handler for DispatcherHandler {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError> {
        self.dispatcher
            .call_conn(cx.conn_id(), method, params, false, emit)
            .map_err(|e| ServiceError {
                code: e.code,
                message: e.message,
                retryable: e.retryable,
            })
    }
    fn on_handoff(&self) {
        if let Some(g) = &self.gate {
            g.request_handoff();
        }
    }
    fn connection_closed(&self, conn: u64) {
        self.dispatcher.connection_closed(conn);
    }
}
