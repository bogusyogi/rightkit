use crate::{JobAssignmentFailure, JobAssignmentMode, ProcessHandle};
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
            OpenThread, ResumeThread, TerminateProcess, CREATE_BREAKAWAY_FROM_JOB,
            CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
        },
    },
};

fn raw(handle: &impl AsHandle) -> HANDLE {
    HANDLE(handle.as_handle().as_raw_handle() as _)
}

fn own(handle: HANDLE) -> OwnedHandle {
    // Each successful Win32 creation returns one newly owned kernel handle.
    unsafe { OwnedHandle::from_raw_handle(handle.0 as _) }
}

pub(crate) fn configure(command: &mut Command, detached: bool, hidden: bool) {
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
    command.creation_flags(flags.0);
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob(OwnedHandle);

impl KillOnCloseJob {
    fn assign(process: &ProcessHandle) -> io::Result<Self> {
        let job = Self(own(
            unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?
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
        .map_err(io::Error::other)?;
        unsafe { AssignProcessToJobObject(raw(&job.0), raw(process)) }.map_err(io::Error::other)?;
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
    owned_job: &mut Option<KillOnCloseJob>,
    failures: &mut Vec<JobAssignmentFailure>,
) -> io::Result<()> {
    // Caller job precedes the empty nested RightKit job. No user code runs yet.
    if let Some(caller) = caller_job {
        assignment_result(
            unsafe { AssignProcessToJobObject(raw(&caller), raw(process)) }
                .map_err(io::Error::other),
            mode,
            "caller job assignment",
            failures,
        )?;
    }
    match KillOnCloseJob::assign(process) {
        Ok(job) => *owned_job = Some(job),
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
        own(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(io::Error::other)?);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe { Thread32First(raw(&snapshot), &mut entry) }.map_err(io::Error::other)?;
    loop {
        if entry.th32OwnerProcessID == pid {
            let thread =
                own(
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                        .map_err(io::Error::other)?,
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
        Some(job) => unsafe { TerminateJobObject(raw(&job.0), code) }.map_err(io::Error::other),
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
            Err(error) => Err(io::Error::other(error)),
        }
    } else {
        Ok(())
    };
    job_result.and(direct_result)
}
