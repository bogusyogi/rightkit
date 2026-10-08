//! Every sandboxed child lives in one kill-on-close, process-capped job; AppContainer
//! confinement (`windows_appcontainer.rs`) adds the filesystem/network boundary on top. The job
//! itself is rightkit-process's public `WindowsJob`.

pub(crate) use rightkit_process::windows_job::{WindowsJob, DEFAULT_ACTIVE_PROCESS_LIMIT};
