//! Windows Job Object lifecycle, ported from CodeRight `windows_job.rs`.
//! AppContainer filesystem/network confinement is NOT ported yet: `Sandbox::spawn`
//! fails closed on Windows when confinement is requested (see matrix.md).
//! Compiled only on Windows; unverified on this macOS host.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

pub const DEFAULT_ACTIVE_PROCESS_LIMIT: u32 = 64;

pub struct WindowsJob {
    handle: HANDLE,
}

unsafe impl Send for WindowsJob {}
unsafe impl Sync for WindowsJob {}

fn err(e: windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(e.code().0)
}

impl WindowsJob {
    pub fn new(active_process_limit: u32) -> std::io::Result<Self> {
        if active_process_limit == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "active process cap must be at least 1",
            ));
        }
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(err)?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = active_process_limit;
        let r = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if let Err(e) = r {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(err(e));
        }
        Ok(Self { handle })
    }

    pub fn assign_process_id(&self, pid: u32) -> std::io::Result<()> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
            .map_err(err)?;
        let r = unsafe { AssignProcessToJobObject(self.handle, process) };
        unsafe {
            let _ = CloseHandle(process);
        }
        r.map_err(err)
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
