use std::io;

/// Route records from the `log` crate into `tracing`, so `log::info!` calls (and dependencies that
/// use `log`) reach the subscriber installed by [`crate::install_tracing`] or the caller's own.
/// Call once, early. Fails if a global `log` logger is already set.
pub fn init_log_bridge() -> io::Result<()> {
    tracing_log::LogTracer::init().map_err(io::Error::other)
}
