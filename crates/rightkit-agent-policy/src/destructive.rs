//! The non-disableable destructive guard from the host app's safety policy. Unlike the permission engine — which
//! a permissive mode (Bypass) can override — this guard hard-blocks a small set
//! of catastrophic, irreversible, whole-system commands *regardless of mode or
//! trust*. It is intentionally conservative: it targets unambiguous
//! system-destroyers, not ordinary risky dev commands.

/// If `command` is catastrophic, return a human-readable reason; else `None`.
pub fn catastrophic_reason(command: &str) -> Option<&'static str> {
    let norm = command.to_lowercase();
    let collapsed = norm.split_whitespace().collect::<Vec<_>>().join(" ");

    // Fork bomb.
    if collapsed.contains(":(){") || collapsed.contains(":|:&") || collapsed.contains(": | : &") {
        return Some("fork bomb");
    }

    // Filesystem creation / raw-disk writes.
    if collapsed.contains("mkfs") || collapsed.contains("wipefs") {
        return Some("filesystem wipe (mkfs/wipefs)");
    }
    if collapsed.contains("of=/dev/sd")
        || collapsed.contains("of=/dev/nvme")
        || collapsed.contains("of=/dev/hd")
        || collapsed.contains("> /dev/sd")
        || collapsed.contains("> /dev/nvme")
    {
        return Some("raw write to a block device");
    }
    if collapsed.contains("shred ") && collapsed.contains("/dev/") {
        return Some("shredding a device");
    }

    // System power state.
    for p in ["shutdown", "reboot", "poweroff", "halt", "init 0", "init 6"] {
        if collapsed == p || collapsed.starts_with(&format!("{p} ")) {
            return Some("system power-state change");
        }
    }

    // Catastrophic recursive delete of root/home.
    if is_catastrophic_rm(&collapsed) {
        return Some("recursive force-delete of a system root");
    }

    None
}

fn is_catastrophic_rm(collapsed: &str) -> bool {
    // Look at each `rm ...` clause (commands may be chained with ; && |).
    for clause in collapsed.split(['\n', ';', '&', '|']) {
        let tokens: Vec<&str> = clause.split_whitespace().collect();
        // `rm` may be prefixed (e.g. `sudo rm`), so match it as any token.
        if !tokens.contains(&"rm") {
            continue;
        }
        let flags: String = tokens
            .iter()
            .filter(|t| t.starts_with('-'))
            .flat_map(|t| t.chars())
            .collect();
        let recursive = flags.contains('r') || flags.contains('R');
        let force = flags.contains('f');
        if collapsed.contains("--no-preserve-root") && recursive {
            return true;
        }
        if !(recursive && force) {
            continue;
        }
        // Targets that name a system root.
        for raw in tokens.iter().skip(1).filter(|t| !t.starts_with('-')) {
            let t = raw.trim_end_matches('/');
            if t.is_empty() || t == "/*" || t == "~" || t == "~/*" || t == "$home" || t == "${home}"
            {
                return true;
            }
        }
        // Bare `rm -rf /` or `rm -rf /*`.
        if tokens.iter().any(|t| *t == "/" || *t == "/*") {
            return true;
        }
    }
    false
}
