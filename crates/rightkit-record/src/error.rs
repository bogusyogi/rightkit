//! The error type shared by every public call. Codes are stable strings that
//! callers can match on.

use std::fmt;
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    ArgInvalid,
    Unsupported,
    PermissionDenied,
    MaskGeometryUnknown,
    WindowHidden,
    WindowMinimized,
    CodecUnavailable,
    OutExists,
    AlreadyRecording,
    NotRecording,
    NotFound,
    NotBundle,
    Helper,
    Io,
}

impl Code {
    /// Every code, in declaration order. Used to read a code back from its string.
    pub const ALL: [Code; 14] = [
        Code::ArgInvalid,
        Code::Unsupported,
        Code::PermissionDenied,
        Code::MaskGeometryUnknown,
        Code::WindowHidden,
        Code::WindowMinimized,
        Code::CodecUnavailable,
        Code::OutExists,
        Code::AlreadyRecording,
        Code::NotRecording,
        Code::NotFound,
        Code::NotBundle,
        Code::Helper,
        Code::Io,
    ];

    /// The code whose stable string is `text`, if any.
    pub fn parse(text: &str) -> Option<Code> {
        Code::ALL.into_iter().find(|code| code.as_str() == text)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Code::ArgInvalid => "arg_invalid",
            Code::Unsupported => "unsupported",
            Code::PermissionDenied => "permission_denied",
            Code::MaskGeometryUnknown => "mask_geometry_unknown",
            Code::WindowHidden => "window_hidden",
            Code::WindowMinimized => "window_minimized",
            Code::CodecUnavailable => "codec_unavailable",
            Code::OutExists => "out_exists",
            Code::AlreadyRecording => "already_recording",
            Code::NotRecording => "not_recording",
            Code::NotFound => "not_found",
            Code::NotBundle => "not_bundle",
            Code::Helper => "helper_failed",
            Code::Io => "io",
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
        RecordError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RecordError {}

pub type Result<T> = std::result::Result<T, RecordError>;

pub(crate) fn io_err(context: &str, err: &io::Error) -> RecordError {
    RecordError::new(Code::Io, format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_through_its_stable_string() {
        for code in Code::ALL {
            assert_eq!(Code::parse(code.as_str()), Some(code));
        }
        assert_eq!(Code::parse("window_hidden"), Some(Code::WindowHidden));
        assert_eq!(
            Code::parse("codec_unavailable"),
            Some(Code::CodecUnavailable)
        );
        assert_eq!(Code::parse("no_such_code"), None);
    }
}
