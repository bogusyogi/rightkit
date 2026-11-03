//! Where the toolkit reports failed Win32 calls. The application installs a sink; until it does,
//! failures are dropped.

use std::sync::OnceLock;

type Sink = Box<dyn Fn(&str, &windows::core::Error, &str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Installs the sink called as `(operation, error, context)`. The first one stays.
pub fn set_error_sink(sink: impl Fn(&str, &windows::core::Error, &str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

pub(crate) fn win32_error(op: &str, error: &windows::core::Error, ctx: &str) {
    if let Some(sink) = SINK.get() {
        sink(op, error, ctx);
    }
}

/// Reports the calling thread's last Win32 error for APIs that only return BOOL or null.
pub(crate) fn last_error(op: &str, ctx: &str) {
    win32_error(op, &windows::core::Error::from_thread(), ctx);
}
