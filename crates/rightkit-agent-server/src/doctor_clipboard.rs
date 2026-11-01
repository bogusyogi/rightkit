// Tiny platform-aware clipboard helper (no external crate). Included from
// `doctor.rs`.

/// List of (binary, args) candidates tried in order when `--clipboard` is set.
///
/// Selection is compile-time by target_os; the actual probe is runtime
/// (each binary is only invoked if it exists on PATH).
pub fn candidate_clipboard_bins() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip.exe", &[])]
    } else {
        // Linux / BSD: try Wayland first, then X11. Skip `termux-clipboard-set`,
        // `cmake-etc-clipboard`, etc — they have wildly-different arg shapes.
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// Best-effort: pipe `text` to the first available system clipboard tool.
///
/// On success: returns the binary name that accepted it.
/// On failure: returns the most recent stderr/exit info; the caller should
/// surface a stderr note but NOT change process exit code.
///
/// No external crate is added — this is intentional (FEATURE-MATRIX par §#37,
/// `clipboard` is best-effort and intentionally lazy).
pub fn try_copy_to_clipboard(text: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;

    let candidates = candidate_clipboard_bins();
    let mut last_err = String::from("no clipboard candidates configured for this target");
    for (bin, args) in candidates {
        let mut cmd = rightkit_child_process::command_std(bin);
        cmd.args(*args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{bin}: spawn error: {e}");
                continue;
            }
        };
        if let Some(stdin) = child.stdin.as_mut() {
            // Swallow broken-pipe: the receiving tool may have exited before
            // we finished writing (e.g. wl-copy when no Wayland compositor).
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(status) if status.success() => return Ok((*bin).to_string()),
            Ok(status) => {
                last_err = format!("{bin}: exited with {status}");
            }
            Err(e) => {
                last_err = format!("{bin}: wait error: {e}");
            }
        }
    }
    Err(last_err)
}
