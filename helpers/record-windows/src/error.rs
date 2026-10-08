//! Typed error codes for the helper. Codes are part of the CLI surface.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    ArgInvalid,
    WindowNotFound,
    WindowHidden,
    WindowMinimized,
    OutDirNotEmpty,
    CaptureUnsupported,
    CaptureFailed,
    /// The requested encoder MFT is not registered or cannot be configured.
    /// Never replaced by a silent fallback to another codec.
    CodecUnavailable,
    Io,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::ArgInvalid => "RK_RECORD_ARG_INVALID",
            Code::WindowNotFound => "RK_RECORD_WINDOW_NOT_FOUND",
            Code::WindowHidden => "RK_RECORD_WINDOW_HIDDEN",
            Code::WindowMinimized => "RK_RECORD_WINDOW_MINIMIZED",
            Code::OutDirNotEmpty => "RK_RECORD_OUT_DIR_NOT_EMPTY",
            Code::CaptureUnsupported => "RK_RECORD_CAPTURE_UNSUPPORTED",
            Code::CaptureFailed => "RK_RECORD_CAPTURE_FAILED",
            Code::CodecUnavailable => "RK_RECORD_CODEC_UNAVAILABLE",
            Code::Io => "RK_RECORD_IO",
        }
    }

    /// 2 for bad arguments, 3 for refusals about the window or output dir, 1 otherwise.
    pub fn exit_code(self) -> i32 {
        match self {
            Code::ArgInvalid => 2,
            Code::WindowNotFound | Code::WindowHidden | Code::WindowMinimized | Code::OutDirNotEmpty => 3,
            Code::CaptureUnsupported | Code::CaptureFailed | Code::CodecUnavailable | Code::Io => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError {
    pub code: Code,
    pub message: String,
}

impl RecordError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RecordError {}
