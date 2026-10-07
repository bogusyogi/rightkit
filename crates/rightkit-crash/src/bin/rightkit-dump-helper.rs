#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("rightkit-dump-helper is Windows-only");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
fn main() {
    std::process::exit(windows::run());
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    struct Args {
        pid: u32,
        output: PathBuf,
    }

    pub(super) fn run() -> i32 {
        let args = match parse_args() {
            Ok(args) => args,
            Err(message) => {
                eprintln!("invalid_arguments: {message}");
                return 2;
            }
        };

        let process =
            unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, args.pid) };
        if process.is_null() {
            eprintln!("open_process_failed: pid={}", args.pid);
            return 3;
        }

        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args.output)
        {
            Ok(file) => file,
            Err(error) => {
                unsafe { CloseHandle(process) };
                eprintln!("create_output_failed: {error}");
                return 4;
            }
        };

        let output_handle: *mut c_void = file.as_raw_handle();
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
            return 5;
        }
        0
    }

    fn parse_args() -> Result<Args, String> {
        let mut pid = None;
        let mut output = None;
        let mut args = std::env::args_os().skip(1);
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
}
