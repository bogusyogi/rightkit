use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CompositionError {
    #[error("unsupported composition schema {0:?}")]
    Schema(String),
    #[error("canvas width, height and frame rate must be positive")]
    Canvas,
    #[error("composition duration must be positive")]
    Duration,
    #[error("layer id {0:?} is empty or duplicated")]
    LayerId(String),
    #[error("layer {layer:?}: {reason}")]
    Invalid { layer: String, reason: String },
    #[error("time {t_ms} ms is outside the composition duration {duration_ms} ms")]
    OutOfRange { t_ms: u64, duration_ms: u64 },
}
