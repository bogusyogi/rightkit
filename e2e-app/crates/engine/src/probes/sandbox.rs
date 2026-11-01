//! rightkit-sandbox: a real child confined to one write root: its write inside lands, its write outside is denied (macOS seatbelt,
//! Linux Landlock); an invalid policy is refused before any spawn. Windows confinement is the app-level e2e's (AppContainer/job).
use serde_json::{json, Value};

#[cfg(unix)]
pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    use rightkit_sandbox::{confine_command, is_landlock_available, NetworkPolicy, SandboxPolicy};
    use std::process::Command;
    if !(cfg!(target_os = "macos") || is_landlock_available()) {
        return Err(
            "needs filesystem confinement: this host has neither seatbelt nor Landlock".into(),
        );
    }
    let root = std::fs::canonicalize(ctx.dir("sandbox")?).map_err(|e| e.to_string())?;
    let (allowed, outside) = (root.join("allowed"), root.join("outside"));
    std::fs::create_dir_all(&allowed).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&outside).map_err(|e| e.to_string())?;
    let policy = SandboxPolicy::new()
        .allow_write(&allowed)
        .network(NetworkPolicy::Allow);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(format!(
            "echo in > '{}/in.txt'; echo out > '{}/out.txt'",
            allowed.display(),
            outside.display()
        ))
        .current_dir(&allowed);
    confine_command(&mut cmd, &policy).map_err(|e| format!("confine: {e}"))?;
    cmd.status().map_err(|e| format!("spawn: {e}"))?;
    let inside = std::fs::read_to_string(allowed.join("in.txt"))
        .map(|s| s == "in\n")
        .unwrap_or(false);
    let denied = !outside.join("out.txt").exists();
    let invalid_refused = confine_command(
        &mut Command::new("/bin/true"),
        &SandboxPolicy::new().allow_read("relative"),
    )
    .is_err();
    if !inside || !denied || !invalid_refused {
        return Err(format!("write inside {inside}, write outside denied {denied}, invalid policy refused {invalid_refused}"));
    }
    Ok(json!({ "inside": inside, "outside_denied": denied, "invalid_refused": invalid_refused }))
}

#[cfg(windows)]
pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    use rightkit_sandbox::SandboxPolicy;
    let invalid_refused = rightkit_sandbox::confine_command(
        &mut std::process::Command::new("cmd"),
        &SandboxPolicy::new().allow_read("relative"),
    )
    .is_err();
    if !invalid_refused {
        return Err("an invalid sandbox policy was accepted".into());
    }
    Ok(
        json!({ "invalid_refused": invalid_refused, "note": "Windows filesystem confinement is exercised by the app-level e2e" }),
    )
}
