use crate::{JobAssignmentFailure, JobAssignmentMode, ProcessHandle, ProcessPriority};
use std::{
    io,
    os::windows::{
        io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::Command,
};
use windows::Win32::{
    Foundation::HANDLE,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            OpenThread, ProcessPowerThrottling, ResumeThread, SetProcessInformation,
            TerminateProcess, BELOW_NORMAL_PRIORITY_CLASS, CREATE_BREAKAWAY_FROM_JOB,
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, DETACHED_PROCESS,
            IDLE_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, THREAD_SUSPEND_RESUME,
        },
    },
};

fn raw(handle: &impl AsHandle) -> HANDLE {
    HANDLE(handle.as_handle().as_raw_handle() as _)
}

pub(crate) fn os_error(error: windows::core::Error) -> io::Error {
    // windows captures GetLastError before cleanup can overwrite it.
    // HRESULT_FROM_WIN32 retains the original error in its low 16 bits.
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error)
    }
}

fn own(handle: HANDLE) -> OwnedHandle {
    // Each successful Win32 creation returns one newly owned kernel handle.
    unsafe { OwnedHandle::from_raw_handle(handle.0 as _) }
}

/// Priority-class creation flag. `Normal` contributes no bits, so the default composition is
/// the one this crate always used.
pub(crate) fn priority_class(priority: ProcessPriority) -> PROCESS_CREATION_FLAGS {
    match priority {
        ProcessPriority::Normal => PROCESS_CREATION_FLAGS(0),
        ProcessPriority::BelowNormal | ProcessPriority::Efficient => BELOW_NORMAL_PRIORITY_CLASS,
        ProcessPriority::Idle => IDLE_PRIORITY_CLASS,
    }
}

/// EcoQoS: ask Windows to run the process on its efficiency class (power throttling of
/// execution speed). The process may still be suspended.
pub(crate) fn apply_efficiency(process: &impl AsHandle) -> io::Result<()> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    // SAFETY: `state` is a valid, live PROCESS_POWER_THROTTLING_STATE of the stated size.
    unsafe {
        SetProcessInformation(
            raw(process),
            ProcessPowerThrottling,
            &state as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(os_error)
}

/// Full creation flags for a spawn: the suspension or breakaway mode and the console flag this
/// crate already sets, with the priority class added. Nothing is removed.
pub(crate) fn creation_flags(
    detached: bool,
    hidden: bool,
    priority: ProcessPriority,
) -> PROCESS_CREATION_FLAGS {
    let mode = if detached {
        CREATE_BREAKAWAY_FROM_JOB
    } else {
        CREATE_SUSPENDED
    };
    let flags = if hidden {
        CREATE_NO_WINDOW | mode
    } else {
        mode
    };
    flags | priority_class(priority)
}

pub(crate) fn configure(
    command: &mut Command,
    detached: bool,
    hidden: bool,
    priority: ProcessPriority,
) {
    command.creation_flags(creation_flags(detached, hidden, priority).0);
}

/// Creation flags of the detached fallback used when the caller's job denies breakaway:
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` plus the priority class. No
/// `CREATE_BREAKAWAY_FROM_JOB`, and no `CREATE_NO_WINDOW` (a detached process has no console, and
/// the two console flags must not be combined).
pub(crate) fn in_job_detached_flags(priority: ProcessPriority) -> PROCESS_CREATION_FLAGS {
    DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | priority_class(priority)
}

/// Replaces the command's creation flags with the in-job detached fallback.
pub(crate) fn configure_in_job_detached(command: &mut Command, priority: ProcessPriority) {
    command.creation_flags(in_job_detached_flags(priority).0);
}

/// Whether a failed breakaway launch was refused by the caller's job object
/// (`ERROR_ACCESS_DENIED`).
pub(crate) fn is_breakaway_denied(error: &io::Error) -> bool {
    error.raw_os_error() == Some(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(flags: PROCESS_CREATION_FLAGS) -> u32 {
        flags.0
    }

    /// The default composition must match what this crate set before the option existed.
    #[test]
    fn default_priority_leaves_the_existing_flags_unchanged() {
        assert_eq!(
            bits(creation_flags(false, true, ProcessPriority::Normal)),
            bits(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        );
        assert_eq!(
            bits(creation_flags(true, true, ProcessPriority::Normal)),
            bits(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        );
        assert_eq!(
            bits(creation_flags(false, false, ProcessPriority::Normal)),
            bits(CREATE_SUSPENDED)
        );
    }

    #[test]
    fn the_in_job_detached_fallback_has_no_breakaway_and_no_window_flag() {
        let flags = bits(in_job_detached_flags(ProcessPriority::Normal));
        assert_eq!(flags, bits(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP));
        assert_eq!(flags & bits(CREATE_BREAKAWAY_FROM_JOB), 0);
        assert_eq!(flags & bits(CREATE_NO_WINDOW), 0);
        let idle = bits(in_job_detached_flags(ProcessPriority::Idle));
        assert_eq!(
            idle,
            bits(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | IDLE_PRIORITY_CLASS)
        );
    }

    #[test]
    fn only_access_denied_counts_as_a_denied_breakaway() {
        assert!(is_breakaway_denied(&io::Error::from_raw_os_error(5)));
        assert!(!is_breakaway_denied(&io::Error::from_raw_os_error(2)));
        assert!(!is_breakaway_denied(&io::Error::other("no code")));
    }

    #[test]
    fn below_normal_keeps_the_console_and_suspension_flags() {
        let flags = creation_flags(false, true, ProcessPriority::BelowNormal);
        assert_eq!(
            bits(flags),
            bits(CREATE_NO_WINDOW | CREATE_SUSPENDED | BELOW_NORMAL_PRIORITY_CLASS)
        );
        assert_eq!(bits(flags) & bits(IDLE_PRIORITY_CLASS), 0);
    }

    #[test]
    fn idle_composes_with_detached_breakaway_and_hidden_console() {
        let flags = creation_flags(true, true, ProcessPriority::Idle);
        assert_eq!(
            bits(flags),
            bits(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB | IDLE_PRIORITY_CLASS)
        );
        assert_eq!(bits(flags) & bits(CREATE_SUSPENDED), 0);
    }

    #[test]
    fn a_visible_child_at_lower_priority_still_has_no_console_flag() {
        let flags = creation_flags(false, false, ProcessPriority::BelowNormal);
        assert_eq!(bits(flags) & bits(CREATE_NO_WINDOW), 0);
        assert_ne!(bits(flags) & bits(BELOW_NORMAL_PRIORITY_CLASS), 0);
    }

    #[test]
    fn efficient_is_below_normal() {
        assert_eq!(
            bits(priority_class(ProcessPriority::Efficient)),
            bits(priority_class(ProcessPriority::BelowNormal))
        );
    }

    #[test]
    fn priority_classes_are_distinct_and_normal_adds_nothing() {
        assert_eq!(bits(priority_class(ProcessPriority::Normal)), 0);
        assert_ne!(
            bits(priority_class(ProcessPriority::BelowNormal)),
            bits(priority_class(ProcessPriority::Idle))
        );
    }
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob(OwnedHandle);

impl KillOnCloseJob {
    fn assign(process: &ProcessHandle) -> io::Result<Self> {
        let job = Self(own(
            unsafe { CreateJobObjectW(None, None) }.map_err(os_error)?
        ));
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                raw(&job.0),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        }
        .map_err(os_error)?;
        unsafe { AssignProcessToJobObject(raw(&job.0), raw(process)) }.map_err(os_error)?;
        Ok(job)
    }
}

fn assignment_result(
    result: io::Result<()>,
    mode: JobAssignmentMode,
    stage: &'static str,
    failures: &mut Vec<JobAssignmentFailure>,
) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if mode == JobAssignmentMode::BestEffort => {
            failures.push(JobAssignmentFailure { stage, error });
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn assign_jobs(
    process: &ProcessHandle,
    caller_job: Option<OwnedHandle>,
    mode: JobAssignmentMode,
    owned_job: &mut Option<std::sync::Arc<KillOnCloseJob>>,
    failures: &mut Vec<JobAssignmentFailure>,
) -> io::Result<()> {
    // Caller job precedes the empty nested RightKit job. No user code runs yet.
    if let Some(caller) = caller_job {
        assignment_result(
            unsafe { AssignProcessToJobObject(raw(&caller), raw(process)) }.map_err(os_error),
            mode,
            "caller job assignment",
            failures,
        )?;
    }
    match KillOnCloseJob::assign(process) {
        Ok(job) => *owned_job = Some(std::sync::Arc::new(job)),
        Err(error) => assignment_result(
            Err(error),
            mode,
            "kill-on-close job setup/assignment",
            failures,
        )?,
    }
    Ok(())
}

/// std closes the primary thread handle after CreateProcessW. The child is
/// still suspended, so its sole initial thread can be reopened safely by ID.
pub(crate) fn resume(pid: u32) -> io::Result<()> {
    let snapshot =
        own(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(os_error)?);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe { Thread32First(raw(&snapshot), &mut entry) }.map_err(os_error)?;
    loop {
        if entry.th32OwnerProcessID == pid {
            let thread =
                own(
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                        .map_err(os_error)?,
                );
            if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        if unsafe { Thread32Next(raw(&snapshot), &mut entry) }.is_err() {
            break;
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "suspended child's primary thread was not found",
    ))
}

pub(crate) fn terminate(
    process: &ProcessHandle,
    job: Option<&KillOnCloseJob>,
    code: u32,
) -> io::Result<()> {
    // Do not skip the job when its original parent already exited.
    let job_result = match job {
        Some(job) => unsafe { TerminateJobObject(raw(&job.0), code) }.map_err(os_error),
        None => Ok(()),
    };
    // Even on failed assignment, preserve direct-child termination/reap.
    let direct_result = if process.try_wait()?.is_none() {
        match unsafe { TerminateProcess(raw(process), code) } {
            Ok(()) => Ok(()),
            // TerminateProcess fails with ACCESS_DENIED while a process is already exiting
            // (job termination is asynchronous): give that exit a bounded moment to land.
            Err(_)
                if process
                    .wait_timeout(std::time::Duration::from_secs(2))?
                    .is_some() =>
            {
                Ok(())
            }
            Err(error) => Err(os_error(error)),
        }
    } else {
        Ok(())
    };
    job_result.and(direct_result)
}
