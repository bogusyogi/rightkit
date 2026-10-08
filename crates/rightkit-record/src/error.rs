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
    OutExists,
    AlreadyRecording,
    NotRecording,
    NotFound,
    NotBundle,
    Helper,
    Io,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::ArgInvalid => "arg_invalid",
            Code::Unsupported => "unsupported",
            Code::PermissionDenied => "permission_denied",
            Code::MaskGeometryUnknown => "mask_geometry_unknown",
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
