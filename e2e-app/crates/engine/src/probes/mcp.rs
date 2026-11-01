//! rightkit-mcp: the trust gate every MCP server passes before it is started: an unknown server is pending, an approved one is
//! approved after a save and reload from disk, a changed environment value invalidates the approval, the fingerprint never
//! contains the secret it covers, and a damaged trust file is an error, never an empty store.
use std::collections::BTreeMap;

use rightkit_mcp::{fingerprint, ServerSpec, TrustDecision, TrustStore};
use serde_json::{json, Value};

fn spec(token: &str) -> ServerSpec {
    ServerSpec::Stdio {
        command: "node".into(),
        args: vec!["server.js".into()],
        env: BTreeMap::from([("API_TOKEN".to_string(), token.to_string())]),
        cwd: Some(std::path::PathBuf::from("/proj")),
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("mcp")?;
    let path = dir.join("trust.json");
    let secret = "sk-live-canary-7f3a9c2e51";
    let mut store = TrustStore::try_load(&path).map_err(|e| format!("load missing: {e}"))?;
    if store.decide("/proj", "srv", &spec(secret)) != TrustDecision::Pending {
        return Err("an unknown server was not pending".into());
    }
    store.approve("/proj", "srv", &spec(secret));
    store.save(&path).map_err(|e| format!("save: {e}"))?;
    let reloaded = TrustStore::try_load(&path).map_err(|e| format!("reload: {e}"))?;
    let approved = reloaded.decide("/proj", "srv", &spec(secret)) == TrustDecision::Approved;
    let changed = reloaded.decide("/proj", "srv", &spec("sk-live-other")) == TrustDecision::Pending;
    let fp = fingerprint(&spec(secret));
    let on_disk = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let leaks = fp.contains(secret) || on_disk.contains(secret);
    std::fs::write(dir.join("damaged.json"), b"{ damaged").map_err(|e| e.to_string())?;
    let damaged_refused = TrustStore::try_load(&dir.join("damaged.json")).is_err();
    if !approved || !changed || leaks || !damaged_refused {
        return Err(format!("approved after reload {approved}, env change invalidates {changed}, secret leaked {leaks}, damaged file refused {damaged_refused}"));
    }
    Ok(
        json!({ "approved_after_reload": approved, "env_change_invalidates": changed, "fingerprint": fp }),
    )
}
