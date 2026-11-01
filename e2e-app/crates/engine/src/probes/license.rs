//! rightkit-license: the cross-process lock the licence state is guarded by: held, then free again after release; the file clock store keeps its state and lock files beside each other.
use rightkit_license::{CrossProcessLock, FileClockStore};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("license")?;
    let lock = dir.join("license.lock");
    let held = CrossProcessLock::acquire(&lock).map_err(|e| format!("acquire: {e}"))?;
    drop(held);
    let free = CrossProcessLock::try_acquire(&lock).map_err(|e| format!("try_acquire after release: {e}"))?.is_some();
    let store = FileClockStore::new(dir.join("clock.json"));
    if !free {
        return Err("the licence lock was not free after release".into());
    }
    Ok(json!({ "free_after_release": free, "clock_lock": store.lock_path() }))
}
