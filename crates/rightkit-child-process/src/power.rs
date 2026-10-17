//! Keep the machine awake while work runs.
//!
//! [`PowerAssertion::acquire`] returns a guard. While it is alive the system does not go to
//! idle sleep (and, when asked, the display stays on); dropping it releases the request.
//! Windows uses `SetThreadExecutionState` on a small holder thread (the state belongs to the
//! calling thread, so a thread that lives exactly as long as the guard owns it). macOS uses an
//! IOKit power assertion (`IOPMAssertionCreateWithName`), which is process-wide. Other hosts
//! return `ErrorKind::Unsupported`: nothing is held and nothing is faked.
//!
//! The request is released if the process dies, by either mechanism. It is advisory for idle
//! sleep: closing a laptop lid or a low battery still sleeps the machine.

use std::io;

/// What to keep awake.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PowerKind {
    /// Prevent idle system sleep; the display may turn off.
    #[default]
    System,
    /// Prevent idle system sleep and keep the display on.
    SystemAndDisplay,
}

/// A live keep-awake request. Released on drop.
#[derive(Debug)]
pub struct PowerAssertion {
    // Held only for its Drop, which releases the request.
    #[allow(dead_code)]
    inner: imp::Inner,
}

impl PowerAssertion {
    /// Take a keep-awake request. `reason` is shown by `pmset -g assertions` on macOS and is
    /// otherwise informational; it must not contain a NUL byte.
    pub fn acquire(reason: &str, kind: PowerKind) -> io::Result<Self> {
        if reason.bytes().any(|byte| byte == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reason contains a NUL byte",
            ));
        }
        Ok(Self {
            inner: imp::Inner::acquire(reason, kind)?,
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::PowerKind;
    use std::{io, sync::mpsc, thread};
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    #[derive(Debug)]
    pub(super) struct Inner {
        release: Option<mpsc::Sender<()>>,
        holder: Option<thread::JoinHandle<()>>,
    }

    impl Inner {
        pub(super) fn acquire(_reason: &str, kind: PowerKind) -> io::Result<Self> {
            let (release, wait) = mpsc::channel::<()>();
            let (ready, started) = mpsc::channel::<io::Result<()>>();
            let holder = thread::Builder::new()
                .name("rightkit-power-assertion".into())
                .spawn(move || {
                    let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
                    if kind == PowerKind::SystemAndDisplay {
                        flags |= ES_DISPLAY_REQUIRED;
                    }
                    // SAFETY: SetThreadExecutionState takes a flag value only. A zero return
                    // means the request was refused.
                    if unsafe { SetThreadExecutionState(flags) }.0 == 0 {
                        let _ = ready.send(Err(io::Error::other(
                            "SetThreadExecutionState refused the request",
                        )));
                        return;
                    }
                    let _ = ready.send(Ok(()));
                    // Returns on release or when the guard (the sender) is gone.
                    let _ = wait.recv();
                    // SAFETY: as above; ES_CONTINUOUS alone clears this thread's request.
                    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                })?;
            match started.recv() {
                Ok(Ok(())) => Ok(Self {
                    release: Some(release),
                    holder: Some(holder),
                }),
                Ok(Err(error)) => {
                    let _ = holder.join();
                    Err(error)
                }
                Err(_) => Err(io::Error::other("power assertion thread ended early")),
            }
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            drop(self.release.take());
            if let Some(holder) = self.holder.take() {
                let _ = holder.join();
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::PowerKind;
    use std::{
        ffi::{c_char, c_void, CString},
        io,
    };

    type CfStringRef = *const c_void;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CfStringRef;
        fn CFRelease(object: *const c_void);
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CfStringRef,
            level: u32,
            name: CfStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    /// Owns one retained CFString.
    struct CfString(CfStringRef);
    impl CfString {
        fn new(text: &str) -> io::Result<Self> {
            let text = CString::new(text)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte"))?;
            // SAFETY: a valid NUL-terminated string and a null (default) allocator.
            let string = unsafe {
                CFStringCreateWithCString(
                    std::ptr::null(),
                    text.as_ptr(),
                    K_CF_STRING_ENCODING_UTF8,
                )
            };
            if string.is_null() {
                return Err(io::Error::other("CFStringCreateWithCString failed"));
            }
            Ok(Self(string))
        }
    }
    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: created by a Create function, released exactly once.
            unsafe { CFRelease(self.0) };
        }
    }

    #[derive(Debug)]
    pub(super) struct Inner {
        id: u32,
    }

    impl Inner {
        pub(super) fn acquire(reason: &str, kind: PowerKind) -> io::Result<Self> {
            let assertion_type = match kind {
                PowerKind::System => "PreventUserIdleSystemSleep",
                PowerKind::SystemAndDisplay => "PreventUserIdleDisplaySleep",
            };
            let assertion_type = CfString::new(assertion_type)?;
            let name = CfString::new(reason)?;
            let mut id = 0u32;
            // SAFETY: both strings outlive the call (IOKit copies them); `id` is a valid out
            // pointer.
            let status = unsafe {
                IOPMAssertionCreateWithName(
                    assertion_type.0,
                    K_IOPM_ASSERTION_LEVEL_ON,
                    name.0,
                    &mut id,
                )
            };
            if status != 0 {
                return Err(io::Error::other(format!(
                    "IOPMAssertionCreateWithName failed: {status:#x}"
                )));
            }
            Ok(Self { id })
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            // SAFETY: `id` came from a successful create and is released once.
            unsafe { IOPMAssertionRelease(self.id) };
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::PowerKind;
    use std::io;

    #[derive(Debug)]
    pub(super) struct Inner;

    impl Inner {
        pub(super) fn acquire(_reason: &str, _kind: PowerKind) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "keep-awake is supported on Windows and macOS only",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_with_a_nul_byte_is_rejected() {
        let error = PowerAssertion::acquire("a\u{0}b", PowerKind::System).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn acquire_and_release_round_trip() {
        let guard = PowerAssertion::acquire("rightkit power test", PowerKind::System).unwrap();
        drop(guard);
        let guard =
            PowerAssertion::acquire("rightkit power test", PowerKind::SystemAndDisplay).unwrap();
        drop(guard);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn other_hosts_report_unsupported() {
        let error = PowerAssertion::acquire("x", PowerKind::System).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
