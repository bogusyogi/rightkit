//! A command-line self-test: scan the arguments for a flag, build a JSON report, write it
//! atomically (or print it) and exit.
//!
//! Release pipelines launch the shipped binary with a self-test flag to prove the packaged build
//! carries the expected product scope. The flag names, the report content and the exit codes' meaning
//! are the host's; this module owns the argument scan and the write.

use std::ffi::OsString;
use std::path::PathBuf;

/// The scan result for one self-test invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestRequest {
    /// The enable flag was present.
    pub enabled: bool,
    /// The value after the output flag, when one was given.
    pub output: Option<PathBuf>,
}

/// Scan `args` (the program name already skipped) for `enable_flag` and `output_flag <path>`.
pub fn scan_args(
    args: impl IntoIterator<Item = OsString>,
    enable_flag: &str,
    output_flag: &str,
) -> SelfTestRequest {
    let mut request = SelfTestRequest {
        enabled: false,
        output: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == enable_flag {
            request.enabled = true;
        } else if arg == output_flag {
            request.output = args.next().map(PathBuf::from);
        }
    }
    request
}

/// Whether the process arguments (program name skipped) contain `flag` exactly.
pub fn has_flag(flag: &str) -> bool {
    std::env::args_os().skip(1).any(|arg| arg == flag)
}

/// Serialize `report` as pretty JSON and write it to `output` (atomically, creating parent
/// folders), or print it on stdout when `output` is `None`. Errors carry `self-test serialize
/// failed: ..` or `self-test write failed: ..`.
pub fn write_report(output: Option<PathBuf>, report: &serde_json::Value) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(report)
        .map_err(|err| format!("self-test serialize failed: {err}"))?;
    match output {
        Some(path) => {
            let options = rightkit_fs::atomic::AtomicWriteOptions {
                create_parents: true,
                ..Default::default()
            };
            rightkit_fs::atomic::atomic_write_with_options(&path, options, |file| {
                std::io::Write::write_all(file, &json)
            })
            .map_err(|err| format!("self-test write failed: {err}"))
        }
        None => {
            println!("{}", String::from_utf8_lossy(&json));
            Ok(())
        }
    }
}

/// Run the self-test when the process was started for it.
///
/// Returns `false` (and does nothing) when `enable_flag` is absent, so the caller continues to
/// start the app. Otherwise `build_report` runs, the report is written, and `true` is returned
/// for the caller to return from `main`. A serialize or write failure prints the error on stderr
/// and exits the process with code 1.
pub fn run(
    enable_flag: &str,
    output_flag: &str,
    build_report: impl FnOnce() -> serde_json::Value,
) -> bool {
    let request = scan_args(std::env::args_os().skip(1), enable_flag, output_flag);
    if !request.enabled {
        return false;
    }
    if let Err(err) = write_report(request.output, &build_report()) {
        eprintln!("{err}");
        std::process::exit(1);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn scans_flag_and_output() {
        let request = scan_args(os(&["--t", "--o", "/x/y.json"]), "--t", "--o");
        assert!(request.enabled);
        assert_eq!(request.output, Some(PathBuf::from("/x/y.json")));
        assert!(!scan_args(os(&["--o", "p"]), "--t", "--o").enabled);
    }

    #[test]
    fn writes_report_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/report.json");
        write_report(Some(path.clone()), &serde_json::json!({"ok": true})).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("\"ok\": true"));
    }
}
