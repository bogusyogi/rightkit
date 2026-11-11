//! Typed errors. `Engine` carries the engine's own `{error:{code,message}}`; the process-call
//! errors come from `rightkit_engine_host::call::CallError`.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("generation engine could not be started at {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("generation engine `{command}` timed out after {seconds}s")]
    Timeout { command: String, seconds: u64 },
    #[error("generation engine `{command}` exited with {code:?} and no JSON result: {stderr}")]
    EngineExit {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("generation engine `{command}` returned unreadable output: {detail}")]
    BadOutput { command: String, detail: String },
    #[error("generation engine error {code}: {message}")]
    Engine { code: String, message: String },
    #[error("generation engine contract version {found} is not supported (need {required}); update the generation engine")]
    ContractVersion { found: u64, required: u64 },
    #[error(
        "generation engine lacks capability {name} v{required} (found {found:?}); update the generation engine"
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
    #[error("request {request_id} has no engine job yet; run recovery")]
    NotSubmitted { request_id: String },
    #[error("request {request_id} was rejected by the engine ({code}): {message}")]
    RequestFailed {
        request_id: String,
        code: String,
        message: String,
    },
    #[error("job {job_id} is not finished ({status})")]
    NotReady { job_id: String, status: String },
    #[error("job {job_id} did not succeed ({status})")]
    JobNotSuccessful { job_id: String, status: String },
    #[error("result of job {job_id} was cleaned up by the engine before it was imported")]
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
    #[error("waiting for job {job_id} timed out; it keeps running in the engine")]
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

impl From<rightkit_engine_host::call::CallError> for Error {
    fn from(e: rightkit_engine_host::call::CallError) -> Self {
        use rightkit_engine_host::call::CallError as C;
        match e {
            C::Spawn { path, source } => Error::Spawn { path, source },
            C::Timeout { command, seconds } => Error::Timeout { command, seconds },
            C::EngineExit {
                command,
                code,
                stderr,
            } => Error::EngineExit {
                command,
                code,
                stderr,
            },
            C::BadOutput { command, detail } => Error::BadOutput { command, detail },
            C::Engine(err) => Error::Engine {
                code: err.code,
                message: err.message,
            },
            C::ContractVersion { found, required } => Error::ContractVersion { found, required },
            C::MissingCapability {
                name,
                required,
                found,
            } => Error::MissingCapability {
                name,
                required,
                found,
            },
        }
    }
}

pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
