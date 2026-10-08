use std::{
    io,
    process::{Child, ExitStatus},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// An independent exit reader. On Unix it shares std's cached reap state;
/// on Windows it owns a duplicated process handle, never a borrowed handle value.
/// Keeping this reader alive does not keep an owned process tree alive.
#[derive(Debug)]
pub struct ProcessHandle {
    #[cfg(unix)]
    child: Arc<Mutex<Child>>,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
    creation_time: Option<SystemTime>,
    #[cfg(windows)]
    creation_time_ticks: Option<u64>,
}

pub(crate) fn lock_child(child: &Mutex<Child>) -> MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl ProcessHandle {
    pub(crate) fn new(child: &Arc<Mutex<Child>>, spawned_at: SystemTime) -> io::Result<Self> {
        #[cfg(unix)]
        {
            #[cfg(target_os = "macos")]
            let creation_time = macos_creation_time(lock_child(child).id());
            #[cfg(not(target_os = "macos"))]
            let creation_time = Some(spawned_at);
            let _ = spawned_at;
            Ok(Self {
                child: Arc::clone(child),
                creation_time,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsHandle;
            let handle = lock_child(child).as_handle().try_clone_to_owned()?;
            let ticks = windows_creation_time(&handle);
            let creation_time = ticks.and_then(|ticks| {
                // FILETIME epoch precedes Unix by 11,644,473,600 seconds.
                let ticks = ticks.checked_sub(116_444_736_000_000_000)?;
                SystemTime::UNIX_EPOCH.checked_add(Duration::new(
                    ticks / 10_000_000,
                    ((ticks % 10_000_000) * 100) as u32,
                ))
            });
            let _ = spawned_at;
            Ok(Self {
                handle,
                creation_time,
                creation_time_ticks: ticks,
            })
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            child: Arc::clone(&self.child),
            #[cfg(windows)]
            handle: self.handle.try_clone()?,
            creation_time: self.creation_time,
            #[cfg(windows)]
            creation_time_ticks: self.creation_time_ticks,
        })
    }

    /// Native creation time on Windows/macOS; spawn observation time on other Unix hosts.
    pub fn creation_time(&self) -> Option<SystemTime> {
        self.creation_time
    }

    /// Native Windows FILETIME, in 100 ns ticks since 1601-01-01 UTC.
    #[cfg(windows)]
    pub fn creation_time_ticks(&self) -> Option<u64> {
        self.creation_time_ticks
    }

    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        #[cfg(unix)]
        {
            lock_child(&self.child).try_wait()
        }
        #[cfg(windows)]
        {
            use std::os::windows::{io::AsRawHandle, process::ExitStatusExt};
            use windows::Win32::{
                Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
                System::Threading::{GetExitCodeProcess, WaitForSingleObject},
            };
            let handle = HANDLE(self.handle.as_raw_handle() as _);
            match unsafe { WaitForSingleObject(handle, 0) } {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    unsafe { GetExitCodeProcess(handle, &mut code) }.map_err(io::Error::other)?;
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    pub fn wait(&self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if started.elapsed() >= timeout {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
        }
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsHandle for ProcessHandle {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        std::os::windows::io::AsHandle::as_handle(&self.handle)
    }
}

#[cfg(windows)]
fn windows_creation_time(handle: &std::os::windows::io::OwnedHandle) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::{FILETIME, HANDLE},
        System::Threading::GetProcessTimes,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(
            HANDLE(handle.as_raw_handle() as _),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(target_os = "macos")]
fn macos_creation_time(pid: u32) -> Option<SystemTime> {
    use nix::libc;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // libproc writes exactly proc_bsdinfo for this flavor; check size before reading it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if written != size as i32 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(info.pbi_start_tvsec))?
        .checked_add(Duration::from_micros(info.pbi_start_tvusec))
}
