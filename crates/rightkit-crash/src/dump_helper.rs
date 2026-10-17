//! The Windows minidump helper as a library entry, so an app ships its own helper binary
//! (`fn main() -> ExitCode { rightkit_crash::dump_helper::run(std::env::args_os().skip(1)) }`)
//! or uses this crate's `rightkit-dump-helper` binary, without writing its own
//! `MiniDumpWriteDump` code. Point [`crate::set_dump_helper`] at the binary.
//!
//! Protocol: `--pid <u32, non-zero> --output <path>`. The output file is created exclusively
//! (an existing file is never overwritten), the dump is synced, and an empty or failed dump is
//! removed. Exit codes: `0` dump written; `2` invalid arguments (or not Windows); `3` the
//! process could not be opened; `4` the output file could not be created; `5` the dump failed.
//! A one-line reason goes to stderr. The Windows call is the only `unsafe` in the crate and
//! lives in a private module of this one.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

/// Process-wide guard: DbgHelp (`MiniDumpWriteDump`) is not thread-safe, so dumps never overlap.
static DUMP_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` holding the process-wide dump lock; a poisoned lock is recovered, never fatal.
fn with_dump_lock<R>(f: impl FnOnce() -> R) -> R {
    let _guard = DUMP_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f()
}

/// Exit code for invalid arguments, and for every non-Windows host.
pub const EXIT_INVALID_ARGUMENTS: u8 = 2;
/// Exit code when the target process could not be opened.
pub const EXIT_OPEN_PROCESS_FAILED: u8 = 3;
/// Exit code when the output file could not be created.
pub const EXIT_CREATE_OUTPUT_FAILED: u8 = 4;
/// Exit code when the dump could not be written, synced, or came out empty.
pub const EXIT_DUMP_FAILED: u8 = 5;

/// Parsed helper arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Process to dump; never zero.
    pub pid: u32,
    /// Dump file to create.
    pub output: PathBuf,
}

/// Parses the arguments after the program name. The error is the text printed after
/// `invalid_arguments: `.
pub fn parse_args<I>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut pid = None;
    let mut output = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--pid") => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing --pid value".to_string())?;
                let parsed = raw
                    .to_str()
                    .ok_or_else(|| "pid is not UTF-8".to_string())?
                    .parse::<u32>()
                    .map_err(|_| "pid is not a positive u32".to_string())?;
                if parsed == 0 {
                    return Err("pid must not be zero".to_string());
                }
                pid = Some(parsed);
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing --output value".to_string())?,
                ));
            }
            Some(other) => return Err(format!("unknown argument {other}")),
            None => return Err("argument flag is not UTF-8".to_string()),
        }
    }
    Ok(Args {
        pid: pid.ok_or_else(|| "--pid is required".to_string())?,
        output: output.ok_or_else(|| "--output is required".to_string())?,
    })
}

/// Runs the helper with the arguments after the program name and returns its exit code.
///
/// Dumps are serialized behind a process-wide lock, because DbgHelp is not thread-safe and
/// concurrent calls can corrupt memory. The lock covers only this crate's own calls: a caller
/// that embeds `run` in a process where other code also uses DbgHelp (symbol handling,
/// stack walking, its own `MiniDumpWriteDump`) must coordinate that code with `run` itself.
pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("invalid_arguments: {message}");
            return ExitCode::from(EXIT_INVALID_ARGUMENTS);
        }
    };
    ExitCode::from(with_dump_lock(|| dump(&args)))
}

#[cfg(not(target_os = "windows"))]
fn dump(_args: &Args) -> u8 {
    eprintln!("rightkit-dump-helper is Windows-only");
    EXIT_INVALID_ARGUMENTS
}

#[cfg(target_os = "windows")]
fn dump(args: &Args) -> u8 {
    win::dump(args)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod win {
    use super::{Args, EXIT_CREATE_OUTPUT_FAILED, EXIT_DUMP_FAILED, EXIT_OPEN_PROCESS_FAILED};
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    pub(super) fn dump(args: &Args) -> u8 {
        // SAFETY: plain value arguments; a null return is checked before use.
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, args.pid) };
        if process.is_null() {
            eprintln!("open_process_failed: pid={}", args.pid);
            return EXIT_OPEN_PROCESS_FAILED;
        }

        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args.output)
        {
            Ok(file) => file,
            Err(error) => {
                // SAFETY: `process` is a live handle owned here and closed exactly once.
                unsafe { CloseHandle(process) };
                eprintln!("create_output_failed: {error}");
                return EXIT_CREATE_OUTPUT_FAILED;
            }
        };

        let output_handle: *mut c_void = file.as_raw_handle();
        // SAFETY: `process` and `output_handle` are live handles for the whole call (`file`
        // outlives it); the optional stream, exception and callback arguments are null, which the
        // API documents as absent.
        let result = unsafe {
            MiniDumpWriteDump(
                process,
                args.pid,
                output_handle,
                MiniDumpNormal | MiniDumpWithThreadInfo,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        // SAFETY: `process` is still the live handle opened above, closed exactly once.
        unsafe { CloseHandle(process) };

        let synced = file.sync_all().is_ok();
        drop(file);
        let nonempty = std::fs::metadata(&args.output)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);
        if result == 0 || !synced || !nonempty {
            let _ = std::fs::remove_file(&args.output);
            eprintln!(
                "dump_failed: write_ok={} sync_ok={synced} nonempty={nonempty}",
                result != 0
            );
            return EXIT_DUMP_FAILED;
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_pid_and_output() {
        let a = parse_args(os(&["--pid", "42", "--output", "x.dmp"])).unwrap();
        assert_eq!(a.pid, 42);
        assert_eq!(a.output, PathBuf::from("x.dmp"));
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            os(&[]),
            os(&["--pid", "1"]),
            os(&["--output", "x"]),
            os(&["--pid", "0", "--output", "x"]),
            os(&["--pid", "-1", "--output", "x"]),
            os(&["--pid", "abc", "--output", "x"]),
            os(&["--pid"]),
            os(&["--pid", "1", "--output"]),
            os(&["--pid", "1", "--output", "x", "--extra"]),
        ] {
            assert!(parse_args(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn run_maps_invalid_arguments_to_exit_2() {
        assert_eq!(
            run(os(&["--bogus"])),
            ExitCode::from(EXIT_INVALID_ARGUMENTS)
        );
    }

    #[test]
    fn dump_lock_serializes_concurrent_callers_and_survives_poison() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        static INSIDE: AtomicUsize = AtomicUsize::new(0);
        static OVERLAPS: AtomicUsize = AtomicUsize::new(0);

        // Poison the lock first; later callers must still proceed.
        let _ = std::thread::spawn(|| with_dump_lock(|| panic!("poison"))).join();

        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    with_dump_lock(|| {
                        if INSIDE.fetch_add(1, Ordering::SeqCst) != 0 {
                            OVERLAPS.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(30));
                        INSIDE.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(OVERLAPS.load(Ordering::SeqCst), 0);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn valid_arguments_off_windows_exit_2_and_write_nothing() {
        let dir = std::env::temp_dir().join(format!("rk-dump-helper-{}", std::process::id()));
        let out = dir.join("x.dmp");
        let code = run(vec![
            OsString::from("--pid"),
            OsString::from("1"),
            OsString::from("--output"),
            out.clone().into_os_string(),
        ]);
        assert_eq!(code, ExitCode::from(EXIT_INVALID_ARGUMENTS));
        assert!(!out.exists());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dumps_this_process_and_refuses_to_overwrite() {
        let dir = std::env::temp_dir();
        let out = dir.join(format!("rk-dump-helper-{}.dmp", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let args = || {
            vec![
                OsString::from("--pid"),
                OsString::from(std::process::id().to_string()),
                OsString::from("--output"),
                out.clone().into_os_string(),
            ]
        };
        assert_eq!(run(args()), ExitCode::SUCCESS);
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        assert_eq!(run(args()), ExitCode::from(EXIT_CREATE_OUTPUT_FAILED));
        let _ = std::fs::remove_file(&out);
    }
}
