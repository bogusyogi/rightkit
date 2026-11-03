//! Typed errors. `Engine` carries GenRight's own `{error:{code,message}}`.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("genright-engine could not be started at {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("genright-engine `{command}` timed out after {seconds}s")]
    Timeout { command: String, seconds: u64 },
    #[error("genright-engine `{command}` exited with {code:?} and no JSON result: {stderr}")]
    EngineExit {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("genright-engine `{command}` returned unreadable output: {detail}")]
    BadOutput { command: String, detail: String },
    #[error("genright-engine error {code}: {message}")]
    Engine { code: String, message: String },
    #[error("genright-engine contract version {found} is not supported (need {required}); update GenRight to the current version")]
    ContractVersion { found: u64, required: u64 },
    #[error(
        "genright-engine lacks capability {name} v{required} (found {found:?}); update GenRight"
    )]
    MissingCapability {
        name: String,
        required: u32,
        found: Option<u32>,
    },
    #[error("invalid {field}: {message}")]
    InvalidParams { field: String, message: String },
    #[error("import journal {path} is unreadable: {detail}")]
    JournalCorrupt { path: PathBuf, detail: String },
    #[error("request {request_id} already exists with a different specification")]
    RequestConflict { request_id: String },
    #[error("request {request_id} is already mapped to job {existing}, refusing job {attempted}")]
    MappingConflict {
        request_id: String,
        existing: String,
        attempted: String,
    },
    #[error("request {request_id} is unknown to this project")]
    UnknownRequest { request_id: String },
    #[error("request {request_id} has no GenRight job yet; run recovery")]
    NotSubmitted { request_id: String },
    #[error("request {request_id} was rejected by GenRight ({code}): {message}")]
    RequestFailed {
        request_id: String,
        code: String,
        message: String,
    },
    #[error("job {job_id} is not finished ({status})")]
    NotReady { job_id: String, status: String },
    #[error("job {job_id} did not succeed ({status})")]
    JobNotSuccessful { job_id: String, status: String },
    #[error("result of job {job_id} was cleaned up by GenRight before it was imported")]
    ResultCleaned { job_id: String },
    #[error("job {job_id} asset {asset_id}: source file missing at {path}")]
    SourceMissing {
        job_id: String,
        asset_id: String,
        path: PathBuf,
    },
    #[error(
        "job {job_id} asset {asset_id}: sha256 mismatch (descriptor {expected}, file {actual})"
    )]
    ShaMismatch {
        job_id: String,
        asset_id: String,
        expected: String,
        actual: String,
    },
    #[error("job {job_id} asset {asset_id}: size mismatch (descriptor {expected}, file {actual})")]
    SizeMismatch {
        job_id: String,
        asset_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("unsafe identifier {0:?}")]
    BadId(String),
    #[error("waiting for job {job_id} timed out; it keeps running in GenRight")]
    WaitTimeout { job_id: String },
    #[error("waiting for job {job_id} was cancelled")]
    WaitCancelled { job_id: String },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
