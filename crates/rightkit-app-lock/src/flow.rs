//! Shell half of the app lock.
//!
//! The shell owns the OS verification (Windows Hello / Touch ID need the window handle).
//! Everything that touches the database or the key is an engine call. These functions take the
//! transport and the OS hooks as arguments so they run without a UI framework in tests. The
//! engine never receives a verify hook.
use serde_json::{json, Value};

use crate::state::AppLockStatus;

/// The engine side of the lock flow: one named call with JSON parameters.
pub trait LockTransport {
    /// Call `method` with `params`; the error is a displayable message.
    fn call(&self, method: &str, params: Value) -> Result<Value, String>;
}

/// The engine method names and the product name the flow prompts with.
#[derive(Debug, Clone, Copy)]
pub struct Methods<'a> {
    /// Product name shown in the verification prompts ("Unlock {product}").
    pub product: &'a str,
    /// Method that sets the lock state (`locked`, `unlocked` or `canceled`).
    pub set_lock: &'a str,
    /// Method that returns the current lock generation as a number.
    pub lock_generation: &'a str,
    /// Method that turns the lock on or off.
    pub set_lock_enabled: &'a str,
}

/// The OS-side hooks. Shell-only by construction.
#[allow(clippy::type_complexity)] // boxed closures are the hook seam
pub struct Shell<'a> {
    /// Whether the stored preference has the lock on (fail closed).
    pub is_enabled: Box<dyn Fn() -> bool + 'a>,
    /// Whether the platform can verify the user.
    pub available: Box<dyn Fn() -> Result<(), String> + 'a>,
    /// Prompt with the given reason; `Ok(false)` means canceled.
    pub verify: Box<dyn Fn(&str) -> Result<bool, String> + 'a>,
}

fn status(result: Result<Value, String>) -> Result<AppLockStatus, String> {
    let value = result?;
    serde_json::from_value(value).map_err(|e| format!("lock status: {e}"))
}

fn generation(engine: &dyn LockTransport, methods: &Methods<'_>) -> Result<u64, String> {
    engine
        .call(methods.lock_generation, json!({}))?
        .as_u64()
        .ok_or_else(|| "lock generation: not a number".to_string())
}

/// Read the generation, prompt, then report the outcome. The prompt runs outside any engine
/// lock, so an auto-lock is never stalled behind it; a lock that lands meanwhile makes the
/// engine discard this unlock.
pub fn unlock(
    engine: &dyn LockTransport,
    methods: &Methods<'_>,
    shell: &Shell<'_>,
) -> Result<AppLockStatus, String> {
    let started = generation(engine, methods)?;
    let reason = format!("Unlock {}", methods.product);
    let request = if (shell.is_enabled)() && !(shell.verify)(&reason)? {
        json!({"state": "canceled", "generation": started})
    } else {
        json!({"state": "unlocked", "generation": started})
    };
    status(engine.call(methods.set_lock, request))
}

/// Lock now; no verification is needed to lock.
pub fn lock(engine: &dyn LockTransport, methods: &Methods<'_>) -> Result<AppLockStatus, String> {
    status(engine.call(methods.set_lock, json!({"state": "locked"})))
}

/// Turn the lock on or off, after the platform check and a verification prompt.
pub fn set_enabled(
    engine: &dyn LockTransport,
    methods: &Methods<'_>,
    shell: &Shell<'_>,
    enabled: bool,
) -> Result<AppLockStatus, String> {
    if enabled {
        (shell.available)()?;
        if !(shell.verify)(&format!("Enable {} app lock", methods.product))? {
            return Err("Verification was canceled.".into());
        }
    } else if (shell.is_enabled)()
        && !(shell.verify)(&format!("Disable {} app lock", methods.product))?
    {
        return Err("Verification was canceled.".into());
    }
    status(engine.call(methods.set_lock_enabled, json!({"enabled": enabled})))
}
