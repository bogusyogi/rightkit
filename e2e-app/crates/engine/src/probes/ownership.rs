//! rightkit-ownership: the real scanner over tiny git fixture repositories. A clean tree is admitted; a tree with a banned
//! source fingerprint (a raw `TcpStream::connect` in code) is refused with the violating file and line; the input cache key
//! moves with the content; the AppKit autorelease-pool rule flags a macOS thread loop with no pool and passes the pooled one.
//!
//! Needs: the `git` executable on PATH (the crate reads git by shelling out). No catalog from the repository is read: the
//! probe supplies a two-capability catalog and an enrolled trust state of its own, as the crate's fixture tests do.
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use rightkit_ownership::keyauth::AuthorityEnv;
use rightkit_ownership::{ownership_cache_key, ManualClock, ScanContext, Scanner};
use serde_json::{json, Value};

const ORIGIN: &str = "https://github.com/test/repo";
const AX_POLL: &str = "pub fn start_focus_observation(running: Arc<AtomicBool>) {\n    std::thread::spawn(move || {\n        while running.load(Ordering::Relaxed) {\n            let system = unsafe { AXUIElementCreateSystemWide() };\n            let mut value: CFTypeRef = std::ptr::null();\n            let status = unsafe { AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute, &mut value) };\n            publish_focus(status, value);\n            std::thread::sleep(Duration::from_millis(100));\n        }\n    });\n}\n";
const POOLED: &str = "use objc2::rc::autoreleasepool;\nuse objc2_app_kit::NSWorkspace;\n\npub fn start_frontmost_poll(tx: Sender<FrontApp>) {\n    std::thread::Builder::new()\n        .name(\"frontmost-poll\".into())\n        .spawn(move || {\n            loop {\n                autoreleasepool(|_| {\n                    let workspace = NSWorkspace::sharedWorkspace();\n                    if let Some(app) = workspace.frontmostApplication() {\n                        let _ = tx.send(FrontApp::from_running(&app));\n                    }\n                });\n                std::thread::sleep(Duration::from_millis(250));\n            }\n        })\n        .expect(\"spawn frontmost poll\");\n}\n";

fn catalog() -> Value {
    json!({
        "schema": 1, "version": "1",
        "capabilities": [
            {"package": "rightkit-http", "capability": "http-client", "tier": "block", "fingerprints": [
                {"id": "http.reqwest", "type": "dependency", "ecosystem": "cargo", "name": "reqwest"},
                {"id": "http.raw-socket", "type": "source", "pattern": "TcpStream::connect", "codeOnly": true, "extensions": [".rs"]}
            ]},
            {"package": "rightkit-ui", "capability": "ui-shell", "fingerprints": [], "detection": "declaration-only"}
        ]
    })
}

fn trust() -> Value {
    json!({
        "trust": {
            "schema": 1, "revokedKeyIds": [], "verifiedTrackingEvidence": [],
            "minimumScannerVersion": "2.0.0", "minimumCatalogVersion": "1",
            "repositories": [{"repositoryIdentity": ORIGIN, "roots": ["."], "visibility": "private"}],
            "hosts": {
                "windows": {"keyId": format!("win-{}", "1".repeat(64)), "publicKeyPem": "TEST-ONLY-PLACEHOLDER-WINDOWS"},
                "macos": {"keyId": format!("mac-{}", "2".repeat(64)), "publicKeyPem": "TEST-ONLY-PLACEHOLDER-MACOS"}
            }
        },
        "enrolled": true, "pending": false, "protection": {"ok": true, "mechanism": "fixture"}
    })
}

fn scanner() -> Scanner {
    let state = trust();
    let ctx = ScanContext {
        clock: Arc::new(ManualClock::new(1_791_504_000_000)),
        host: "e2e-host/e2e-os".into(),
        authority: AuthorityEnv::unavailable(),
        trust_loader: Arc::new(move || Ok(state.clone())),
        policy_digest: "e".repeat(64),
        before_inventory: None,
    };
    Scanner::new(ctx, catalog())
}

fn git(root: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .env("GIT_AUTHOR_NAME", "E2E")
        .env("GIT_AUTHOR_EMAIL", "e2e@example.invalid")
        .env("GIT_COMMITTER_NAME", "E2E")
        .env("GIT_COMMITTER_EMAIL", "e2e@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .output()
        .map_err(|e| format!("needs the git executable on PATH: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)))
    }
}

fn repo(base: &Path, name: &str, main_rs: &str) -> Result<std::path::PathBuf, String> {
    let root = base.join(name);
    std::fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;
    git(&root, &["init", "-q", "-b", "main"])?;
    git(&root, &["remote", "add", "origin", ORIGIN])?;
    let w = |rel: &str, text: &str| std::fs::write(root.join(rel), text).map_err(|e| format!("write {rel}: {e}"));
    w("Cargo.toml", "[package]\nname = \"fixture-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")?;
    w("Cargo.lock", "# This file is automatically @generated by Cargo.\nversion = 3\n\n[[package]]\nname = \"fixture-app\"\nversion = \"0.1.0\"\n")?;
    w("src/main.rs", main_rs)?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-q", "-m", "fixture"])?;
    // The scanner canonicalises its root; do the same so the verbatim/symlinked temp prefix cannot differ.
    std::fs::canonicalize(&root).map_err(|e| e.to_string())
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let base = ctx.dir("ownership")?;
    let clean = repo(&base, "clean", "fn main() {}\n")?;
    let dirty = repo(&base, "dirty", "fn main() {\n    let _ = std::net::TcpStream::connect(\"x\");\n}\n")?;
    let s = scanner();

    let r = s.scan(&clean);
    if let Some(code) = r.get("code") {
        return Err(format!("scan of the clean tree errored: {code} {}", r["error"]));
    }
    if r["verdict"]["admit"] != json!(true) || r["verdict"]["code"] != json!("RK_OWNERSHIP_CLEAN") {
        return Err(format!("clean tree not admitted: {}", r["verdict"]));
    }
    if !r["report"]["violations"].as_array().is_some_and(|v| v.is_empty()) {
        return Err("clean tree reported violations".into());
    }

    let d = s.scan(&dirty);
    if let Some(code) = d.get("code") {
        return Err(format!("scan of the violating tree errored: {code} {}", d["error"]));
    }
    if d["verdict"]["admit"] != json!(false) || d["verdict"]["code"] != json!("RK_OWNERSHIP_VIOLATION") {
        return Err(format!("violating tree not refused: {}", d["verdict"]));
    }
    let v = d["report"]["violations"].as_array().and_then(|v| v.first()).ok_or("no violation recorded")?;
    if v["fingerprint"] != json!("http.raw-socket") || v["file"] != json!("src/main.rs") || v["line"] != json!(2) || v["package"] != json!("rightkit-http") {
        return Err(format!("violation details wrong: {v}"));
    }

    // The probe (inputs) side: a deterministic key that moves with the content.
    let ki = s.inputs(&clean);
    let kd = s.inputs(&dirty);
    let key_clean = ownership_cache_key(&ki["inputs"]).map_err(|e| format!("cache key: {} {}", e.code, e.message))?;
    let key_clean_again = ownership_cache_key(&s.inputs(&clean)["inputs"]).map_err(|e| e.message)?;
    let key_dirty = ownership_cache_key(&kd["inputs"]).map_err(|e| e.message)?;
    if key_clean != key_clean_again || key_clean == key_dirty {
        return Err("the cache key is not stable for equal content or does not change with content".into());
    }

    // Native AppKit autorelease-pool rule.
    let bare = rightkit_ownership::autorelease::findings("src/macos/focus.rs", AX_POLL.as_bytes(), "rightkit-ownership");
    let pooled = rightkit_ownership::autorelease::findings("src/macos/poll.rs", POOLED.as_bytes(), "rightkit-ownership");
    if bare.len() != 1 || !pooled.is_empty() {
        return Err(format!("autorelease rule: {} findings on the bare loop, {} on the pooled one", bare.len(), pooled.len()));
    }

    Ok(json!({
        "clean": r["verdict"]["code"],
        "violation": {"fingerprint": v["fingerprint"], "file": v["file"], "line": v["line"]},
        "cache_key_prefix": &key_clean[..key_clean.len().min(12)],
        "autorelease_findings": bare.len(),
    }))
}
