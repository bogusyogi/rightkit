use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CompositionError {
    #[error("unsupported composition schema {0:?}")]
    Schema(String),
    #[error("canvas width and height must be positive")]
    Canvas,
    #[error("frame rate numerator and denominator must be positive")]
    FrameRate,
    #[error("composition duration must be positive")]
    Duration,
    #[error("composition JSON: {0}")]
    Json(String),
    #[error("legacy composition conversion: {0}")]
    Legacy(String),
    #[error("layer id {0:?} is empty or duplicated")]
    LayerId(String),
    #[error("layer {layer:?}: {reason}")]
    Invalid { layer: String, reason: String },
    #[error("time {t} is outside the composition duration {duration}")]
    OutOfRange {
        t: crate::ticks::Ticks,
        duration: crate::ticks::Ticks,
    },
}
