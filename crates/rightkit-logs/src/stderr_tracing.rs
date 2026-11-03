//! Stderr-only `tracing` initializer (feature `tracing`).

use std::io::{self, Write};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

/// Collects one formatted event (message, fields and span context) and, when the event's writer
/// is dropped, writes it to stderr after the crate scrubber has run over the whole text. Every
/// stderr error is swallowed (a closed or broken stderr, as under a GUI launch or a closed pipe),
/// so logging can never fail or panic the caller.
#[derive(Debug, Default)]
pub(crate) struct LossyStderr {
    buf: Vec<u8>,
}

/// The `MakeWriter` for [`LossyStderr`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScrubbedStderr;

impl LossyStderr {
    fn emit(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.buf);
        let scrubbed = crate::scrub::scrub_text(&text);
        self.buf.clear();
        let mut err = io::stderr();
        let _ = err.write_all(scrubbed.as_bytes());
        let _ = err.flush();
    }
}

impl Write for LossyStderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LossyStderr {
    fn drop(&mut self) {
        self.emit();
    }
}

impl<'a> MakeWriter<'a> for ScrubbedStderr {
    type Writer = LossyStderr;

    fn make_writer(&'a self) -> Self::Writer {
        LossyStderr::default()
    }
}

/// The filter for `RUST_LOG` (when set and valid), else `default_filter`, else `info`.
pub(crate) fn stderr_filter(env: Option<&str>, default_filter: &str) -> EnvFilter {
    env.and_then(|value| EnvFilter::try_new(value).ok())
        .or_else(|| EnvFilter::try_new(default_filter).ok())
        .unwrap_or_else(|| EnvFilter::new("info"))
}

/// Install a global `tracing` subscriber that writes human-readable lines to stderr only.
///
/// The filter is `RUST_LOG` when set and valid, otherwise `default_filter` (for example
/// `"info"`); an invalid `default_filter` falls back to `info`. Write errors on stderr are
/// ignored. Fails only when a global subscriber is already installed.
pub fn install_stderr_tracing(default_filter: &str) -> io::Result<()> {
    let env = std::env::var("RUST_LOG").ok();
    let filter = stderr_filter(env.as_deref(), default_filter);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(ScrubbedStderr)
        .with_ansi(false);
    Registry::default()
        .with(filter)
        .with(layer)
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))
}
