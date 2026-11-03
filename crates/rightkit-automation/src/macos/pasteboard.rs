//! macOS general pasteboard: every type's raw bytes, and a change watcher. AppKit posts no
//! notification when the pasteboard changes, so the watcher reads `changeCount` on a short timer
//! from its own thread. The cross-platform surface is [`crate::clipboard`].

use super::ffi::*;
use crate::clipboard::{RawClipboard, RawFormat, RawLimits};
use crate::model::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Pasteboard types a copying app sets to say "do not record this": the nspasteboard.org
/// concealed type (password managers) and 1Password's own marker.
const PRIVATE_TYPES: [&str; 2] = [
    "org.nspasteboard.ConcealedType",
    "com.agilebits.onepassword",
];

unsafe fn general() -> Id {
    msg0(class(c"NSPasteboard"), c"generalPasteboard")
}

unsafe fn msg_bytes(obj: Id, s: &std::ffi::CStr) -> *const u8 {
    let f: unsafe extern "C" fn(Id, Sel) -> *const u8 =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}

/// The pasteboard change count: bumped by every write.
pub fn sequence() -> u64 {
    pool(|| unsafe { msg_i64(general(), c"changeCount") as u64 })
}

fn read_once(limits: &RawLimits) -> RawClipboard {
    pool(|| unsafe {
        let pb = general();
        let count_before = msg_i64(pb, c"changeCount") as u64;
        let types = msg0(pb, c"types");
        let n = if types.is_null() {
            0
        } else {
            msg_i64(types, c"count").max(0) as usize
        };
        let names: Vec<String> = (0..n)
            .filter_map(|i| ns_to_string(msg_idx(types, c"objectAtIndex:", i)))
            .collect();
        let mut out = RawClipboard {
            sequence: count_before,
            formats: Vec::new(),
            skipped: Vec::new(),
            concealed: false,
        };
        if names.iter().any(|t| PRIVATE_TYPES.contains(&t.as_str())) {
            out.concealed = true;
            out.skipped = names
                .into_iter()
                .map(|t| format!("{t} (withheld: the copying app marked the content as private)"))
                .collect();
            return out;
        }
        let mut total = 0usize;
        for name in names {
            if total >= limits.max_total_bytes {
                out.skipped
                    .push(format!("{name} (total byte limit reached)"));
                continue;
            }
            let data = msg1(pb, c"dataForType:", ns_string(&name));
            if data.is_null() {
                out.skipped.push(format!("{name} (no data)"));
                continue;
            }
            let len = msg_i64(data, c"length").max(0) as usize;
            let p = msg_bytes(data, c"bytes");
            if p.is_null() || len == 0 {
                out.skipped.push(format!("{name} (empty)"));
                continue;
            }
            let room = limits.max_format_bytes.min(limits.max_total_bytes - total);
            let take = len.min(room);
            let bytes = std::slice::from_raw_parts(p, take).to_vec();
            total += take;
            out.formats.push(RawFormat {
                name,
                bytes,
                truncated: len > take,
            });
        }
        out
    })
}

/// Every pasteboard type with its bytes. Retries when the change count moved during the read.
pub fn read_raw(limits: &RawLimits) -> Result<RawClipboard> {
    for _ in 0..3 {
        let r = read_once(limits);
        if sequence() == r.sequence {
            return Ok(r);
        }
    }
    Err(Error::Platform(
        0,
        "the pasteboard kept changing during the read",
    ))
}

/// A running watcher. Dropping it stops the thread and joins it.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Poll interval: short enough that a change is reported within a human-imperceptible delay,
/// long enough that a `changeCount` read (one message send) costs nothing measurable.
const POLL: Duration = Duration::from_millis(100);

pub fn watch(on_change: Box<dyn Fn(u64) + Send>) -> Result<Watcher> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("rightkit-pasteboard-watch".into())
        .spawn(move || {
            let mut last = sequence();
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(POLL);
                let now = sequence();
                if now != last {
                    last = now;
                    // A panic in the caller's callback must not end the watcher.
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_change(now)));
                }
            }
        })
        .map_err(|_| Error::Platform(0, "spawn pasteboard watcher thread"))?;
    Ok(Watcher {
        stop,
        thread: Some(thread),
    })
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
