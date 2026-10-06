//! Safety gate: per-app allowlist plus a process-wide kill switch. Every act
//! entry point consults this before touching an app.

use crate::model::{Error, Result};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static KILLED: AtomicBool = AtomicBool::new(false);

/// Flip from a hotkey handler / UI button / signal handler; acts fail closed.
pub fn kill() {
    KILLED.store(true, Ordering::SeqCst);
}
pub fn reset_kill() {
    KILLED.store(false, Ordering::SeqCst);
}
pub fn killed() -> bool {
    KILLED.load(Ordering::SeqCst)
}

#[derive(Default)]
pub struct Policy {
    allowed: Mutex<HashSet<String>>,
}

impl Policy {
    /// `allow` holds bundle ids (macOS) or exe names (Windows). Empty denies all.
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(allow: I) -> Self {
        Policy { allowed: Mutex::new(allow.into_iter().map(Into::into).collect()) }
    }
    pub fn check(&self, app_id: &str) -> Result<()> {
        if killed() {
            return Err(Error::Blocked("kill switch engaged".into()));
        }
        if self.allowed.lock().unwrap().contains(app_id) {
            Ok(())
        } else {
            Err(Error::Blocked(format!("{app_id} is not on the allowlist")))
        }
    }
}
