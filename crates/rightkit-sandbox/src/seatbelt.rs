//! macOS Seatbelt backend (ported from CodeRight `macos_seatbelt.rs`).
//!
//! Lessons carried over: a `(deny default)` profile MUST start with
//! `(import "bsd.sb")` or children SIGABRT before `main`; rules must name
//! realpath-resolved paths (`/tmp` -> `/private/tmp`). Plain children get the
//! profile in-process via `sandbox_init` in `pre_exec` (no argv leak); PTY
//! children use the `sandbox-exec -p` wrapper because `portable-pty` exposes
//! no `pre_exec`.

use crate::{NetworkPolicy, SandboxPolicy};
use std::ffi::{CString, OsString};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn sandbox_init(profile: *const libc::c_char, flags: u64, errorbuf: *mut *mut libc::c_char) -> i32;
    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

/// Resolve symlinks; for paths that do not exist yet, resolve the deepest existing ancestor.
pub fn resolve_for_profile(path: &Path) -> PathBuf {
    if let Ok(r) = std::fs::canonicalize(path) {
        return r;
    }
    let mut trailing = Vec::new();
    let mut cursor = path;
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            trailing.push(name.to_owned());
        }
        if let Ok(mut out) = std::fs::canonicalize(parent) {
            for part in trailing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

fn quote(path: &Path) -> String {
    let mut out = String::from("\"");
    for c in path.to_string_lossy().chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

const SYSTEM_READ: &[&str] = &["/usr", "/System", "/Library", "/etc", "/private/etc", "/private/var/db", "/dev", "/bin", "/sbin", "/Applications"];
const SYSTEM_EXEC: &[&str] = &["/usr", "/bin", "/sbin", "/System", "/Library", "/Applications"];

/// Build the deny-default SBPL profile for `policy`.
pub fn profile_for(policy: &SandboxPolicy) -> String {
    let mut p = String::from("(version 1)\n(import \"bsd.sb\")\n(deny default)\n(allow process-fork)\n(allow signal (target self))\n(allow sysctl-read)\n");
    for dir in SYSTEM_EXEC {
        p.push_str(&format!("(allow process-exec* (subpath \"{dir}\"))\n"));
    }
    for e in &policy.exec_paths {
        let r = quote(&resolve_for_profile(e));
        p.push_str(&format!("(allow process-exec* (subpath {r}))\n(allow file-read* (subpath {r}))\n"));
    }
    match policy.network {
        NetworkPolicy::Allow => p.push_str("(allow network*)\n"),
        NetworkPolicy::Deny => p.push_str("(deny network-outbound)\n(allow network-outbound (remote unix-socket))\n"),
    }
    for dir in SYSTEM_READ {
        p.push_str(&format!("(allow file-read* (subpath \"{dir}\"))\n"));
    }
    // Root directory entries and metadata are needed for path walking.
    p.push_str("(allow file-read-metadata)\n(allow file-read* (literal \"/\"))\n");
    for r in &policy.read_paths {
        p.push_str(&format!("(allow file-read* (subpath {}))\n", quote(&resolve_for_profile(r))));
    }
    for w in &policy.write_paths {
        let r = quote(&resolve_for_profile(w));
        p.push_str(&format!("(allow file-read* file-write* (subpath {r}))\n"));
    }
    p.push_str("(allow file-read* file-write* file-ioctl (regex #\"^/dev/(null|zero|tty|ttys[0-9]+|dtracehelper|urandom|random)$\"))\n");
    p
}

/// Prepared in the parent; call [`Prepared::apply`] from `pre_exec`.
pub struct Prepared(CString);

impl Prepared {
    pub fn new(policy: &SandboxPolicy) -> std::io::Result<Self> {
        CString::new(profile_for(policy)).map(Self).map_err(|_| std::io::Error::other("seatbelt profile contained a NUL byte"))
    }

    /// Apply to the current process. Intended only for a forked child before exec.
    pub fn apply(&self) -> std::io::Result<()> {
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated profile; err is a valid out-pointer freed below.
        let status = unsafe { sandbox_init(self.0.as_ptr(), 0, &mut err) };
        if status == 0 {
            return Ok(());
        }
        let detail = if err.is_null() {
            "unknown error".to_string()
        } else {
            // SAFETY: sandbox_init populated a NUL-terminated string, freed exactly once.
            let t = unsafe { std::ffi::CStr::from_ptr(err) }.to_string_lossy().into_owned();
            unsafe { sandbox_free_error(err) };
            t
        };
        Err(std::io::Error::other(format!("sandbox_init: {detail}")))
    }
}

/// `sandbox-exec -p <profile> program args...` wrapper (used for PTY children).
pub fn wrap_argv(policy: &SandboxPolicy, program: &OsString, args: &[OsString]) -> (OsString, Vec<OsString>) {
    let mut out = vec![OsString::from("-p"), OsString::from(profile_for(policy)), program.clone()];
    out.extend(args.iter().cloned());
    (OsString::from("/usr/bin/sandbox-exec"), out)
}
