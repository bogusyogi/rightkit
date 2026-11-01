//! rightkit-credentials: a round trip through the OS credential store (Keychain / Credential Manager), non-interactive, under a
//! namespace unique to this run: set, read back, conditional delete, gone. A store that would prompt is a failure (it must not prompt).
use rightkit_credentials::{Config, Namespace, Secret, Store};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let run = ctx
        .data
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("run")
        .to_string();
    let ns = Namespace::new(format!("app.rightsuite.rke2e.probe.{}", std::process::id()))
        .map_err(|e| format!("namespace: {e}"))?;
    let store = Store::open(Config::new(ns)).non_interactive();
    let account = format!("probe-{run}");
    let value = "probe-secret-value";
    store
        .set_str(&account, value)
        .map_err(|e| format!("set: {e}"))?;
    let got = store
        .get(&account)
        .map_err(|e| format!("get: {e}"))?
        .ok_or("the secret was not found after set")?;
    let same = got.expose_str().map(|s| s == value).unwrap_or(false);
    let deleted = store
        .delete_if_equals(&account, &Secret::from_string(value))
        .map_err(|e| format!("conditional delete: {e}"))?;
    // macOS Keychain has no atomic compare-and-delete: the store says Unsupported and removes nothing (its contract), so the
    // probe then deletes plainly; every other outcome must already have removed the item.
    let plain = if matches!(
        deleted,
        rightkit_credentials::ConditionalDelete::Unsupported
    ) {
        Some(store.delete(&account).map_err(|e| format!("delete: {e}"))?)
    } else {
        None
    };
    let gone = !store.exists(&account).map_err(|e| format!("exists: {e}"))?;
    if !same || !gone {
        return Err(format!(
            "round trip failed: read back equal={same}, gone after delete={gone}"
        ));
    }
    Ok(
        json!({ "durable": store.is_durable(), "read_back": same, "delete": format!("{deleted:?}"), "plain_delete": plain, "gone": gone }),
    )
}
