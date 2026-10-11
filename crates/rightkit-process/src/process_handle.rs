use std::{
    io,
    process::ExitStatus,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime},
};
#[cfg(unix)]
type PlatformChild = crate::unix_child::UnixChild;
#[cfg(windows)]
type PlatformChild = crate::spawn_windows::WindowsChild;
#[cfg(unix)]
use std::time::Instant;

/// An independent exit reader. On Unix it shares the owner's cached reap state;
/// on Windows it owns a duplicated process handle, never a borrowed handle value.
/// Keeping this reader alive does not keep an owned process tree alive.
///
/// Unix: `wait`, `try_wait` and `wait_timeout` reap the leader when it has exited. The first
/// reap by any path, this reader or the owner, permanently ends process-group signalling for
/// the `OwnedChild` and every clone: the group id may be recycled from then on, so nothing is
/// sent to it again. A reader that reaps therefore also means the owner can no longer kill
/// descendants that outlive the leader.
///
/// [`wait_exited`](Self::wait_exited) is the monitor's call: it returns once the leader has
/// exited **without reaping it**, so the owner can still terminate the group (the leader's
/// zombie pins the group id) and then reap. A monitor thread that only needs to learn of the
/// exit, to wake a run loop for instance, should use it, and leave the reap to the owner.
#[derive(Debug)]
pub struct ProcessHandle {
    #[cfg(unix)]
    child: Arc<Mutex<PlatformChild>>,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
    creation_time: Option<SystemTime>,
    #[cfg(windows)]
    creation_time_ticks: Option<u64>,
}

pub(crate) fn lock_child<T>(child: &Mutex<T>) -> MutexGuard<'_, T> {
    child.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl ProcessHandle {
    pub(crate) fn new(
        child: &Arc<Mutex<PlatformChild>>,
        spawned_at: SystemTime,
    ) -> io::Result<Self> {
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
            self.windows_wait(0)
        }
    }

    /// Blocks until the leader exits and reaps it (Unix) or until the process exits (Windows).
    ///
    /// Unix: a loop of non-blocking `try_wait` looks. Each look takes the shared lock and
    /// releases it at once; between looks the thread sleeps, ending early at the child's exit
    /// where the kernel offers an event (a hint, see `exit_watch`) and otherwise after 5 ms. The
    /// lock is never held while this thread sleeps, so `kill`, `terminate_tree` and other
    /// waiters run meanwhile (unless some other thread holds an `OwnedChild::child()` guard,
    /// which is the lock itself), and no wait system call names the pid outside the lock: a
    /// blocked `waitid(pid)` entered after another reader reaped the leader and the kernel
    /// recycled the number would wait on an unrelated child.
    pub fn wait(&self) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            self.wait_until(None)?
                .ok_or_else(|| io::Error::other("infinite process wait unexpectedly timed out"))
        }
        #[cfg(windows)]
        {
            self.windows_wait(u32::MAX)?
                .ok_or_else(|| io::Error::other("infinite process wait unexpectedly timed out"))
        }
    }

    /// The wait shared by `wait` and `wait_timeout`: `Ok(None)` when `deadline` passed first.
    #[cfg(unix)]
    fn wait_until(&self, deadline: Option<Instant>) -> io::Result<Option<ExitStatus>> {
        crate::unix_child::wait_shared(&self.child, deadline)
    }

    /// Blocks until the leader has exited, and does not reap it.
    ///
    /// Unix: returns when the leader is a zombie (or was already reaped by someone). The zombie
    /// still pins the pid and the process-group id, so the owner can keep signalling the group
    /// (`OwnedChild::wait_or_kill`, `terminate_tree*`) and then reap. This is the call for an
    /// exit monitor that must not end the owner's ability to kill descendants that outlive the
    /// leader; `wait` and `try_wait` reap and do end it. Waits like [`wait`](Self::wait), with
    /// the non-reaping `waitid(WNOHANG | WNOWAIT)` probe as its look. Hosts with no verified
    /// non-reaping probe (Unix other than Linux, Android, macOS and iOS) have nothing to
    /// observe with, so there this reaps exactly like `wait`.
    ///
    /// Windows: returns once the process has exited; nothing is reaped there.
    pub fn wait_exited(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            if !crate::unix_child::CAN_OBSERVE_WITHOUT_REAPING {
                return self.wait().map(|_| ());
            }
            self.observe_exit(None).map(|_| ())
        }
        #[cfg(windows)]
        {
            self.windows_wait(u32::MAX)?
                .map(|_| ())
                .ok_or_else(|| io::Error::other("infinite process wait unexpectedly timed out"))
        }
    }

    /// [`wait_exited`](Self::wait_exited) with a limit: `true` once the leader has exited
    /// (without reaping it on Linux, Android, macOS and iOS), `false` if `timeout` passed first.
    /// For a caller that must enforce a deadline and still keep the group killable while it
    /// drains the leader's pipes; it reaps afterwards with `wait`. Hosts with no non-reaping
    /// probe behave like [`wait_timeout`](Self::wait_timeout).
    pub fn wait_exited_timeout(&self, timeout: Duration) -> io::Result<bool> {
        #[cfg(unix)]
        {
            if !crate::unix_child::CAN_OBSERVE_WITHOUT_REAPING {
                return self.wait_timeout(timeout).map(|status| status.is_some());
            }
            self.observe_exit(Some(timeout))
        }
        #[cfg(windows)]
        {
            self.wait_timeout(timeout).map(|status| status.is_some())
        }
    }

    /// Looks at the leader without reaping it until it has exited or `limit` has passed.
    #[cfg(unix)]
    fn observe_exit(&self, limit: Option<Duration>) -> io::Result<bool> {
        let deadline = limit.and_then(|limit| Instant::now().checked_add(limit));
        crate::unix_child::observe_shared(&self.child, deadline)
    }

    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        #[cfg(windows)]
        {
            let started = std::time::Instant::now();
            loop {
                let remaining = timeout.saturating_sub(started.elapsed());
                // Round up sub-millisecond waits; reserve INFINITE for wait().
                let millis = remaining.as_millis()
                    + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
                let millis = millis.min(u128::from(u32::MAX - 1)) as u32;
                if let Some(status) = self.windows_wait(millis)? {
                    return Ok(Some(status));
                }
                if started.elapsed() >= timeout {
                    return Ok(None);
                }
            }
        }
        #[cfg(unix)]
        {
            self.wait_until(Instant::now().checked_add(timeout))
        }
    }

    #[cfg(windows)]
    fn windows_wait(&self, millis: u32) -> io::Result<Option<ExitStatus>> {
        use std::os::windows::{io::AsRawHandle, process::ExitStatusExt};
        use windows::Win32::{
            Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        };
        let handle = HANDLE(self.handle.as_raw_handle() as _);
        match unsafe { WaitForSingleObject(handle, millis) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(handle, &mut code) }
                    .map_err(crate::process_windows::os_error)?;
                Ok(Some(ExitStatus::from_raw(code)))
            }
            _ => Err(io::Error::last_os_error()),
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
