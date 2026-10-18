//! A public kill-on-close Windows Job Object for processes the caller already spawned. Closing
//! the last handle (drop) kills every process in the job; an optional active-process cap bounds
//! fork bombs. rightkit-sandbox confines its children with this; apps attach it to a
//! `std::process::Child` they spawned themselves.

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

use crate::process_windows::os_error;

/// The active-process cap rightkit-sandbox applies to sandboxed trees.
pub const DEFAULT_ACTIVE_PROCESS_LIMIT: u32 = 64;

/// Kill-on-close job. Every assigned process dies when this value is dropped.
pub struct WindowsJob {
    handle: HANDLE,
    job_memory_limit: Option<usize>,
}

/// Why the processes in a job stopped, as far as the job can tell. See
/// [`WindowsJob::exit_reason`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The job's peak committed memory stayed below its job memory limit, or there was none.
    Exited,
    /// The job's peak committed memory reached its job memory limit. The OS reports only that
    /// peak, not which process was refused memory, so this is evidence that the cap was hit.
    JobMemoryLimit,
}

/// Limits for a [`WindowsJob`]: an active-process cap and/or memory caps. Created by
/// [`WindowsJob::builder`]; every limit is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowsJobBuilder {
    active_process_limit: Option<u32>,
    job_memory_limit: Option<usize>,
    process_memory_limit: Option<usize>,
}

impl WindowsJobBuilder {
    /// Caps the number of live processes in the job at `cap` (at least 1).
    pub fn active_process_limit(mut self, cap: u32) -> Self {
        self.active_process_limit = Some(cap);
        self
    }

    /// Caps the total committed memory of all processes in the job at `bytes` (at least 1).
    /// Allocations past the cap fail in the process that makes them.
    pub fn job_memory_limit(mut self, bytes: usize) -> Self {
        self.job_memory_limit = Some(bytes);
        self
    }

    /// Caps the committed memory of each process in the job at `bytes` (at least 1).
    pub fn process_memory_limit(mut self, bytes: usize) -> Self {
        self.process_memory_limit = Some(bytes);
        self
    }

    /// Creates the job with these limits.
    pub fn build(self) -> std::io::Result<WindowsJob> {
        if self.active_process_limit == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "active process cap must be at least 1",
            ));
        }
        if self.job_memory_limit == Some(0) || self.process_memory_limit == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "memory limits must be at least 1 byte",
            ));
        }
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(os_error)?;
        let job = WindowsJob {
            handle,
            job_memory_limit: self.job_memory_limit,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(cap) = self.active_process_limit {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = cap;
        }
        if let Some(bytes) = self.job_memory_limit {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            limits.JobMemoryLimit = bytes;
        }
        if let Some(bytes) = self.process_memory_limit {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            limits.ProcessMemoryLimit = bytes;
        }
        unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(os_error)?;
        Ok(job)
    }
}

// SAFETY: a job handle is a kernel object handle, usable from any thread.
unsafe impl Send for WindowsJob {}
unsafe impl Sync for WindowsJob {}

impl WindowsJob {
    /// Creates the job. `Some(n)` caps the number of live processes in it at `n` (at least 1);
    /// `None` leaves the count unbounded.
    pub fn new(active_process_limit: Option<u32>) -> std::io::Result<Self> {
        WindowsJobBuilder {
            active_process_limit,
            ..WindowsJobBuilder::default()
        }
        .build()
    }

    /// A builder for a job with memory caps as well as the active-process cap.
    pub fn builder() -> WindowsJobBuilder {
        WindowsJobBuilder::default()
    }

    /// Puts the current process into a new kill-on-close job and leaks the job handle, so every
    /// process that descends from this one (and this one) is terminated when the process exits,
    /// including an abnormal exit. Nothing is changed when the call fails. Best effort: when an
    /// outer job forbids nesting the assignment fails with `PermissionDenied`, and the caller may
    /// ignore that error. Call it once, early.
    pub fn contain_current_process() -> std::io::Result<()> {
        let job = WindowsJob::new(None)?;
        // The pseudo-handle from GetCurrentProcess names this process with full access.
        let process = unsafe { GetCurrentProcess() };
        unsafe { AssignProcessToJobObject(job.handle, process) }.map_err(os_error)?;
        // Leaked on purpose: the handle stays open for the process lifetime, so kill-on-close
        // fires only when the process itself ends.
        std::mem::forget(job);
        Ok(())
    }

    /// The peak committed memory of all processes that were in the job, in bytes.
    pub fn peak_job_memory(&self) -> std::io::Result<usize> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(self.handle),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(os_error)?;
        Ok(limits.PeakJobMemoryUsed)
    }

    /// Reports [`ExitReason::JobMemoryLimit`] when the job's peak committed memory reached the
    /// job memory limit it was built with, otherwise [`ExitReason::Exited`]. Read it after the
    /// processes have exited; the job handle keeps the peak until it closes.
    pub fn exit_reason(&self) -> std::io::Result<ExitReason> {
        match self.job_memory_limit {
            Some(limit) if self.peak_job_memory()? >= limit => Ok(ExitReason::JobMemoryLimit),
            _ => Ok(ExitReason::Exited),
        }
    }

    /// Assigns an already-spawned child through its own process handle.
    pub fn assign_child(&self, child: &Child) -> std::io::Result<()> {
        let process = HANDLE(child.as_raw_handle());
        unsafe { AssignProcessToJobObject(self.handle, process) }.map_err(os_error)
    }

    /// Assigns a process by id; needs `PROCESS_SET_QUOTA | PROCESS_TERMINATE` on it.
    pub fn assign_process_id(&self, pid: u32) -> std::io::Result<()> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
            .map_err(os_error)?;
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process) };
        unsafe {
            let _ = CloseHandle(process);
        }
        assigned.map_err(os_error)
    }

    /// Kills every process in the job now.
    pub fn terminate(&self) -> std::io::Result<()> {
        unsafe { TerminateJobObject(self.handle, 1) }.map_err(os_error)
    }

    /// How many processes are currently active in the job. Observation only; the count is
    /// a snapshot and can change as soon as it is read.
    pub fn active_process_count(&self) -> std::io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(self.handle),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(os_error)?;
        Ok(accounting.ActiveProcesses)
    }

    /// Whether no process is currently active in the job (see
    /// [`active_process_count`](Self::active_process_count)).
    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.active_process_count()? == 0)
    }

    /// Wait at most `timeout` until every process in this Job has exited.
    ///
    /// The Job remains owned by `self` on timeout, so callers can keep its
    /// kill-on-close guarantee while retaining any related cleanup leases.
    pub fn wait_empty(&self, timeout: Duration) -> std::io::Result<bool> {
        const POLL_INTERVAL: Duration = Duration::from_millis(10);
        let started = Instant::now();
        loop {
            if self.is_empty()? {
                return Ok(true);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            thread::sleep(POLL_INTERVAL.min(remaining));
        }
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
