use rightkit_ort::ort;
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Ort(ort::Error),
    Audio(String),
    Model(String),
    Tokenizer(String),
    Config(String),
    /// Long-form decode stopped by the caller's cancellation flag.
    Cancelled,
    /// A Whisper run stopped by the caller's [`CancelToken`](crate::whisper::CancelToken)
    /// or by a deadline. Carries the windows that had completed. Use
    /// [`Error::is_cancelled`] to test for a caller cancel and [`Error::interrupted`]
    /// to reach the partial transcript.
    Interrupted(Box<crate::whisper::WhisperInterrupted>),
    /// A single-window decode received more audio than the model window allows
    /// (`window_samples` plus one mel hop). Use the long-form route instead.
    WindowTooLong { samples: usize, max_samples: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO error: {e}"),
            Error::Ort(e) => write!(f, "ONNX Runtime error: {e}"),
            Error::Audio(msg) => write!(f, "Audio processing error: {msg}"),
            Error::Model(msg) => write!(f, "Model error: {msg}"),
            Error::Tokenizer(msg) => write!(f, "Tokenizer error: {msg}"),
            Error::Config(msg) => write!(f, "Config error: {msg}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Interrupted(i) => {
                let what = match i.reason {
                    crate::whisper::InterruptReason::Cancelled => "cancelled",
                    crate::whisper::InterruptReason::DeadlineExceeded => "deadline exceeded",
                };
                write!(
                    f,
                    "whisper run {what} after {} of about {} window(s); partial transcript kept",
                    i.windows_completed, i.windows_estimated
                )
            }
            Error::WindowTooLong { samples, max_samples } => write!(
                f,
                "window of {samples} samples exceeds the {max_samples}-sample limit (model window + one hop); use transcribe_long"
            ),
        }
    }
}

impl Error {
    /// True for a caller cancel: [`Error::Cancelled`] or an [`Error::Interrupted`]
    /// whose reason is `Cancelled`.
    pub fn is_cancelled(&self) -> bool {
        match self {
            Error::Cancelled => true,
            Error::Interrupted(i) => i.reason == crate::whisper::InterruptReason::Cancelled,
            _ => false,
        }
    }

    /// The interruption details (reason and partial transcript) for an
    /// [`Error::Interrupted`].
    pub fn interrupted(&self) -> Option<&crate::whisper::WhisperInterrupted> {
        match self {
            Error::Interrupted(i) => Some(i),
            _ => None,
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl<R> From<ort::Error<R>> for Error
where
    ort::Error<R>: Into<ort::Error<()>>,
{
    fn from(e: ort::Error<R>) -> Self {
        Error::Ort(e.into())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Config(e.to_string())
    }
}

impl From<hound::Error> for Error {
    fn from(e: hound::Error) -> Self {
        Error::Audio(e.to_string())
    }
}
