// Per-request memory call context (deadline + cancellation).
// The former `/memright/*` compatibility routes and their OpenAPI entries are removed:
// memory now lives behind Pulse's Membrane CLI (Orthic-Labs/pulse#26), reached only
// through the `coderight-membrane-cli` adapter. The request context stays because every
// memory and context call must carry a deadline.

use crate::backend::CallContext;
use axum::{extract::Request, middleware::Next, response::Response};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MEMORY_REQUEST_DEADLINE: Duration = Duration::from_secs(30);

tokio::task_local! {
    static MEMORY_REQUEST_CONTEXT: CallContext;
}

struct MemoryRequestGuard {
    cancellation: CancellationToken,
}

impl Drop for MemoryRequestGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Bind one request-owned cancellation/deadline context around all daemon
/// routes. Dropping an aborted Axum request cancels in-flight backend calls.
pub async fn memory_request_context(request: Request, next: Next) -> Response {
    let cancellation = CancellationToken::new();
    let context =
        CallContext::after(MEMORY_REQUEST_DEADLINE).with_cancellation(cancellation.clone());
    let _guard = MemoryRequestGuard { cancellation };
    MEMORY_REQUEST_CONTEXT
        .scope(context, next.run(request))
        .await
}

pub fn current_memory_call_context() -> CallContext {
    MEMORY_REQUEST_CONTEXT
        .try_with(Clone::clone)
        .unwrap_or_else(|_| CallContext::after(MEMORY_REQUEST_DEADLINE))
}
