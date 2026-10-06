//! NSWorkspace / NSRunningApplication / NSPasteboard through the Objective-C runtime.

use super::ffi::*;
use crate::model::*;
use crate::platform::ClipSnapshot;

const PLAIN: &str = "public.utf8-plain-text";

pub fn apps() -> Vec<AppInfo> {
    pump();
    pool(|| unsafe {
        let ws = msg0(class(c"NSWorkspace"), c"sharedWorkspace");
        let arr = msg0(ws, c"runningApplications");
        let n = msg_i64(arr, c"count");
        let mut out = vec![];
        for i in 0..n as usize {
            let a = msg_idx(arr, c"objectAtIndex:", i);
            // 0 = regular (Dock) apps; accessory/prohibited apps are not drivable UIs.
            if msg_i64(a, c"activationPolicy") != 0 || msg_bool(a, c"isTerminated") {
                continue;
            }
            if let Some(info) = info_of(a).filter(|i| alive(i.pid)) {
                out.push(info);
            }
        }
        out
    })
}

unsafe fn info_of(a: Id) -> Option<AppInfo> {
    let pid = msg_i64(a, c"processIdentifier") as i32;
    if pid <= 0 {
        return None;
    }
    let name = ns_to_string(msg0(a, c"localizedName")).unwrap_or_default();
    let bundle = ns_to_string(msg0(a, c"bundleIdentifier")).filter(|s| !s.is_empty());
    let path = ns_to_string(msg0(msg0(a, c"bundleURL"), c"path"));
    let launch = msg0(a, c"launchDate");
    let started = (!launch.is_null()).then(|| (msg_f64(launch, c"timeIntervalSince1970") * 1000.0) as u64);
    Some(AppInfo { pid, app_id: bundle.unwrap_or_else(|| name.to_ascii_lowercase()), name, path, started })
}

pub fn app(pid: i32) -> Option<AppInfo> {
    pump();
    pool(|| unsafe {
        let f: unsafe extern "C" fn(Id, Sel, i32) -> Id = std::mem::transmute(objc_msgSend as *const ());
        let a = f(class(c"NSRunningApplication"), sel(c"runningApplicationWithProcessIdentifier:"), pid);
        if a.is_null() || msg_bool(a, c"isTerminated") {
            None
        } else {
            info_of(a).filter(|i| alive(i.pid))
        }
    })
}

pub fn frontmost_pid() -> Option<i32> {
    pump();
    pool(|| unsafe {
        let ws = msg0(class(c"NSWorkspace"), c"sharedWorkspace");
        let a = msg0(ws, c"frontmostApplication");
        (!a.is_null()).then(|| msg_i64(a, c"processIdentifier") as i32)
    })
}

/// AXFrontmost on the app element, then NSRunningApplication activation.
pub fn activate(pid: i32) -> bool {
    let ax = super::ax::El::app(pid).and_then(|e| e.set_bool("AXFrontmost", true)).is_ok();
    let ns = pool(|| unsafe {
        let f: unsafe extern "C" fn(Id, Sel, i32) -> Id = std::mem::transmute(objc_msgSend as *const ());
        let a = f(class(c"NSRunningApplication"), sel(c"runningApplicationWithProcessIdentifier:"), pid);
        !a.is_null() && msg_bool1_u64(a, c"activateWithOptions:", 1)
    });
    ax || ns
}

pub fn terminate(pid: i32) -> bool {
    pool(|| unsafe {
        let f: unsafe extern "C" fn(Id, Sel, i32) -> Id = std::mem::transmute(objc_msgSend as *const ());
        let a = f(class(c"NSRunningApplication"), sel(c"runningApplicationWithProcessIdentifier:"), pid);
        !a.is_null() && msg_bool(a, c"terminate")
    })
}

/// The bundle id `spec` will run as, decided before anything is launched. A bundle
/// id is itself; an `.app` path is resolved from its Info.plist (via `plutil`, which
/// reads XML and binary plists). Unresolvable specs fail closed.
pub fn resolve_launch(spec: &str) -> Result<String> {
    let target = spec.strip_prefix("hidden:").unwrap_or(spec);
    if !(target.contains('/') || target.ends_with(".app")) {
        return Ok(target.to_string());
    }
    let plist = std::path::Path::new(target).join("Contents/Info.plist");
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .map_err(|e| Error::NotFound(format!("plutil: {e}")))?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || id.is_empty() {
        return Err(Error::NotFound(format!("bundle id of {target} (is it an .app bundle?)")));
    }
    Ok(id)
}

/// `open -g [-j] -n`: background, optionally hidden, always a NEW instance so
/// the run only ever terminates a process it started itself.
pub fn launch(spec: &str) -> Result<i32> {
    let (hidden, target) = match spec.strip_prefix("hidden:") {
        Some(t) => (true, t),
        None => (false, spec),
    };
    let before: std::collections::HashSet<i32> = apps().into_iter().map(|a| a.pid).collect();
    let mut cmd = std::process::Command::new("/usr/bin/open");
    cmd.arg("-g").arg("-n");
    if hidden {
        cmd.arg("-j");
    }
    if target.contains('/') || target.ends_with(".app") {
        cmd.arg(target);
    } else {
        cmd.args(["-b", target]);
    }
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err(Error::NotFound(format!("could not open {target}")));
    }
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let newest = apps().into_iter().find(|a| {
            !before.contains(&a.pid) && (a.app_id.eq_ignore_ascii_case(target) || a.path.as_deref().is_some_and(|p| target.contains('/') && p.trim_end_matches('/') == target.trim_end_matches('/')))
        });
        if let Some(a) = newest {
            return Ok(a.pid);
        }
    }
    Err(Error::NotFound(format!("no new process appeared for {target} (already running apps are never adopted)")))
}

// ---------------------------------------------------------------- pasteboard

fn lossless_type(t: &str) -> bool {
    matches!(
        t,
        "public.utf8-plain-text" | "NSStringPboardType" | "public.utf16-external-plain-text" | "public.utf16-plain-text" | "com.apple.traditional-mac-plain-text"
    ) || t.starts_with("org.nspasteboard.")
}

pub fn clip_change() -> u64 {
    pool(|| unsafe { msg_i64(msg0(class(c"NSPasteboard"), c"generalPasteboard"), c"changeCount") as u64 })
}

pub fn clip_snapshot() -> Option<ClipSnapshot> {
    pool(|| unsafe {
        let pb = msg0(class(c"NSPasteboard"), c"generalPasteboard");
        let change = msg_i64(pb, c"changeCount") as u64;
        let types = msg0(pb, c"types");
        let n = if types.is_null() { 0 } else { msg_i64(types, c"count") };
        let mut lossless = true;
        for i in 0..n as usize {
            if let Some(t) = ns_to_string(msg_idx(types, c"objectAtIndex:", i)) {
                lossless &= lossless_type(&t);
            }
        }
        let text = ns_to_string(msg1(pb, c"stringForType:", ns_string(PLAIN)));
        Some(ClipSnapshot { text, lossless, change, formats: n as usize })
    })
}

pub fn clip_write(text: &str) -> Option<u64> {
    pool(|| unsafe {
        let pb = msg0(class(c"NSPasteboard"), c"generalPasteboard");
        msg_i64(pb, c"clearContents");
        let ok = msg_bool2(pb, c"setString:forType:", ns_string(text), ns_string(PLAIN));
        ok.then(|| msg_i64(pb, c"changeCount") as u64)
    })
}

pub fn clip_restore(snap: &ClipSnapshot, expected: u64) -> bool {
    pool(|| unsafe {
        let pb = msg0(class(c"NSPasteboard"), c"generalPasteboard");
        if msg_i64(pb, c"changeCount") as u64 != expected {
            return false;
        }
        msg_i64(pb, c"clearContents");
        match &snap.text {
            Some(t) => msg_bool2(pb, c"setString:forType:", ns_string(t), ns_string(PLAIN)),
            None => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Admission needs the id BEFORE launch; a path spec must resolve from the real plist.
    #[test]
    fn path_and_hidden_specs_resolve_without_launching() {
        assert_eq!(resolve_launch("/System/Applications/Calculator.app").unwrap(), "com.apple.calculator");
        assert_eq!(resolve_launch("hidden:/System/Applications/Calculator.app").unwrap(), "com.apple.calculator");
        assert_eq!(resolve_launch("hidden:com.apple.TextEdit").unwrap(), "com.apple.TextEdit");
        assert!(resolve_launch("/nonexistent/Nope.app").is_err(), "unresolvable specs fail closed");
    }
}
