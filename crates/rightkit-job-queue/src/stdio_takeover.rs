//! Stdio takeover for an engine that speaks frames on stdin and stdout.
//!
//! [`take_frame_io`] moves the process's real stdin and stdout into private reader and writer
//! objects and puts harmless handles in their place, so a child process the engine starts (a
//! shell, `open`, `ssh`, an uninstaller) or a stray `println!` can neither read the requests nor
//! write into the frames. Hand the pair to [`crate::serve`] / [`crate::serve_report`].
//!
//! Unix: the real descriptors are duplicated (close-on-exec) and fd 0 becomes `/dev/null`, fd 1
//! the stderr descriptor. Windows: the real handles are duplicated as non-inheritable handles and
//! the originals closed; the standard input becomes `NUL` and the standard output the stderr
//! handle, which is what `Stdio::inherit` copies into a child. Any other target gets the standard
//! streams unchanged.
//!
//! Call it once, early in `main`, before anything else writes to stdout. It declares the raw OS
//! calls itself (no extra dependency); this module holds the only `unsafe` outside `ffi`.

use std::io::{BufRead, Write};

/// The frame reader and writer, with stdin and stdout taken over (see the module documentation).
/// Fails when a standard stream is not open or the OS refuses the swap; nothing is changed then.
pub fn take_frame_io() -> std::io::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
    imp::take()
}

#[cfg(unix)]
mod imp {
    use std::io::{BufRead, Write};
    use std::os::fd::{AsFd, AsRawFd};

    unsafe extern "C" {
        fn dup2(old: i32, new: i32) -> i32;
    }

    pub(super) fn take() -> std::io::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let input = std::io::stdin().as_fd().try_clone_to_owned()?;
        let output = std::io::stdout().as_fd().try_clone_to_owned()?;
        let null = std::fs::File::open("/dev/null")?;
        let stderr = std::io::stderr();
        // SAFETY: both targets are the process's own standard descriptors and the sources stay
        // open for the calls.
        let swapped =
            unsafe { dup2(null.as_raw_fd(), 0) != -1 && dup2(stderr.as_raw_fd(), 1) != -1 };
        if !swapped {
            return Err(std::io::Error::last_os_error());
        }
        Ok((
            Box::new(std::io::BufReader::new(std::fs::File::from(input))),
            Box::new(std::fs::File::from(output)),
        ))
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io::{BufRead, Read, Write};
    use std::os::windows::io::{FromRawHandle, RawHandle};

    type Handle = *mut c_void;

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: usize = usize::MAX;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn GetCurrentProcess() -> Handle;
        fn DuplicateHandle(
            source_process: Handle,
            source: Handle,
            target_process: Handle,
            target: *mut Handle,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn CloseHandle(object: Handle) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            attributes: *const c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
    }

    fn usable(handle: Handle) -> bool {
        !handle.is_null() && handle as usize != INVALID_HANDLE_VALUE
    }

    /// A pipe whose writer has closed reads as end of input, as stdin does.
    struct Pipe(std::fs::File);

    impl Read for Pipe {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(0),
                other => other,
            }
        }
    }

    /// A non-inheritable copy of `source` in this process.
    ///
    /// SAFETY: `source` is a live handle of this process.
    unsafe fn duplicate(source: Handle) -> std::io::Result<Handle> {
        let mut copy: Handle = std::ptr::null_mut();
        let process = GetCurrentProcess();
        if DuplicateHandle(
            process,
            source,
            process,
            &mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(copy)
    }

    /// Returns non-inheritable copies of the real stdin and stdout, closes the originals and
    /// points the standard input at NUL and the standard output at the stderr handle.
    fn take_standard_handles() -> std::io::Result<(usize, usize)> {
        // SAFETY: the handles are this process's own standard handles; every handle opened here
        // is returned to the caller, installed as a standard handle, or closed.
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE);
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let stderr = GetStdHandle(STD_ERROR_HANDLE);
            if !usable(stdin) || !usable(stdout) || !usable(stderr) {
                return Err(std::io::Error::other(
                    "a standard handle is missing (stdin, stdout and stderr must all be open)",
                ));
            }
            let input = duplicate(stdin)?;
            let output = match duplicate(stdout) {
                Ok(output) => output,
                Err(error) => {
                    CloseHandle(input);
                    return Err(error);
                }
            };
            let name: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
            let null = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if !usable(null) {
                let error = std::io::Error::last_os_error();
                CloseHandle(input);
                CloseHandle(output);
                return Err(error);
            }
            if SetStdHandle(STD_INPUT_HANDLE, null) == 0
                || SetStdHandle(STD_OUTPUT_HANDLE, stderr) == 0
            {
                let error = std::io::Error::last_os_error();
                SetStdHandle(STD_INPUT_HANDLE, stdin);
                SetStdHandle(STD_OUTPUT_HANDLE, stdout);
                CloseHandle(input);
                CloseHandle(output);
                CloseHandle(null);
                return Err(error);
            }
            // The originals may be inheritable (the supervisor passed them down): a child would
            // receive them with every other inheritable handle, so close them. Never close the
            // stderr handle, which stdout now is, and close a shared handle once.
            if stdin != stderr {
                CloseHandle(stdin);
            }
            if stdout != stderr && stdout != stdin {
                CloseHandle(stdout);
            }
            Ok((input as usize, output as usize))
        }
    }

    pub(super) fn take() -> std::io::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let (input, output) = take_standard_handles()?;
        // SAFETY: both are fresh handles this function owns; nothing else closes them.
        let (input, output) = unsafe {
            (
                std::fs::File::from_raw_handle(input as RawHandle),
                std::fs::File::from_raw_handle(output as RawHandle),
            )
        };
        Ok((
            Box::new(std::io::BufReader::new(Pipe(input))),
            Box::new(output),
        ))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io::{BufRead, Write};

    pub(super) fn take() -> std::io::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        Ok((
            Box::new(std::io::BufReader::new(std::io::stdin())),
            Box::new(std::io::stdout()),
        ))
    }
}
