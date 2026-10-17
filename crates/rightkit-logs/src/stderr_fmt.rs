//! A stderr `tracing` layer that composes with other layers (feature `tracing`).
//!
//! Unlike [`crate::install_tracing`], nothing here installs a global subscriber. The caller adds
//! the layer to its own registry, next to a file layer or a telemetry layer.

use crate::stderr_tracing::ScrubbedStderr;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

const DEFAULT_DIRECTIVE: &str = "info";

/// A formatted stderr layer with its own filter: `RUST_LOG` when it is set, otherwise
/// `default_filter`, otherwise `info`. Each complete event is scrubbed with [`crate::scrub_text`]
/// before it is written, and write errors are ignored. It is not a global install; add it to a
/// registry with `SubscriberExt::with`, next to other layers. An invalid `default_filter` falls
/// back to `info`.
pub fn stderr_layer<S>(default_filter: &str) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_filter))
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_DIRECTIVE));
    fmt::layer()
        .with_writer(ScrubbedStderr)
        .with_filter(filter)
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn stderr_layer_composes_with_a_registry_without_installing_one() {
        let subscriber = tracing_subscriber::registry()
            .with(stderr_layer::<tracing_subscriber::Registry>("warn"));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("below the warn default, so not written");
            tracing::warn!("written to stderr by the composed layer");
        });
    }
}
