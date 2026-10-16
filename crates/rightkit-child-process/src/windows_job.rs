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
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

use crate::process_windows::os_error;

/// The active-process cap rightkit-sandbox applies to sandboxed trees.
pub const DEFAULT_ACTIVE_PROCESS_LIMIT: u32 = 64;

/// Kill-on-close job. Every assigned process dies when this value is dropped.
pub struct WindowsJob {
    handle: HANDLE,
}

// SAFETY: a job handle is a kernel object handle, usable from any thread.
unsafe impl Send for WindowsJob {}
unsafe impl Sync for WindowsJob {}

impl WindowsJob {
    /// Creates the job. `Some(n)` caps the number of live processes in it at `n` (at least 1);
    /// `None` leaves the count unbounded.
    pub fn new(active_process_limit: Option<u32>) -> std::io::Result<Self> {
        if active_process_limit == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "active process cap must be at least 1",
            ));
        }
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(os_error)?;
        let job = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(cap) = active_process_limit {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = cap;
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

    /// Wait at most `timeout` until every process in this Job has exited.
    ///
    /// The Job remains owned by `self` on timeout, so callers can keep its
    /// kill-on-close guarantee while retaining any related cleanup leases.
    pub fn wait_empty(&self, timeout: Duration) -> std::io::Result<bool> {
        const POLL_INTERVAL: Duration = Duration::from_millis(10);
        let started = Instant::now();
        loop {
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
            if accounting.ActiveProcesses == 0 {
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
