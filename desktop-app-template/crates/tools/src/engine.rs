//! Build the engine sidecar through managed RightKit Cargo and stage it where Tauri's
//! `bundle.externalBin` expects it: `src-tauri/binaries/<name>-<target-triple>[.exe]`.
//! Managed builds put output under an external target root, so the executable comes from
//! cargo's own JSON artifact stream rather than a guessed path.
use crate::{read, Result};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

fn engine_name(root: &Path) -> Result<String> {
    let toml = read(root, "crates/engine/Cargo.toml")?;
    let bin = toml.split("[[bin]]").nth(1).ok_or("engine Cargo.toml has no [[bin]]")?;
    bin.lines()
        .find_map(|l| l.trim().strip_prefix("name").and_then(|r| r.split('"').nth(1)).map(String::from))
        .ok_or_else(|| "engine [[bin]] has no name".into())
}

fn rightkit(root: &Path) -> Command {
    let mut c = Command::new("rightkit");
    c.current_dir(root);
    c
}

pub fn build(root: &Path) -> Result<()> {
    let engine = engine_name(root)?;
    let target = match std::env::var("TAURI_ENV_TARGET_TRIPLE") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            let out = rightkit(root).args(["rustc", "-vV"]).output().map_err(|e| format!("rightkit rustc -vV: {e}"))?;
            if !out.status.success() {
                return Err(format!("rightkit rustc -vV failed ({})", out.status));
            }
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("host:").map(|h| h.trim().to_string()))
                .ok_or("could not determine the Rust host triple")?
        }
    };
    let profile: Vec<String> = if std::env::var("RIGHTKIT_BUILD_MODE").as_deref() == Ok("dev") {
        vec!["--profile".into(), std::env::var("RIGHTKIT_CARGO_PROFILE").unwrap_or_else(|_| "release-iterate".into())]
    } else {
        vec!["--release".into()]
    };
    let manifest = root.join("Cargo.toml");
    let out = rightkit(root)
        .args(["cargo", "build"])
        .args(&profile)
        .args(["--locked", "--target", &target, "--message-format=json-render-diagnostics", "--manifest-path"])
        .arg(&manifest)
        .args(["-p", &engine])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("rightkit cargo build: {e}"))?;
    if !out.status.success() {
        return Err(format!("{engine} build failed ({})", out.status));
    }
    let mut source = None;
    for line in String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.starts_with('{')) {
        if let Ok(m) = serde_json::from_str::<Value>(line) {
            if m["reason"] == "compiler-artifact" && m["target"]["name"] == engine.as_str() {
                if let Some(exe) = m["executable"].as_str() {
                    source = Some(exe.to_string());
                }
            }
        }
    }
    let source = source.ok_or_else(|| format!("{engine} build produced no executable"))?;
    let dir = root.join("src-tauri").join("binaries");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(format!("{engine}-{target}{}", if target.contains("windows") { ".exe" } else { "" }));
    std::fs::copy(&source, &dest).map_err(|e| format!("stage {source}: {e}"))?;
    println!("staged {source} -> {}", dest.display());
    Ok(())
}
