//! What a sharing host says about this computer: its name and its model word.

/// The computer's name. On macOS it is `scutil --get ComputerName` (then `hostname`), read by
/// running a program, so it is kept for five minutes: a rename still shows without a process start
/// per heartbeat; `Mac` when neither program answers. Elsewhere it is `COMPUTERNAME` or `HOSTNAME`,
/// else `PC`.
pub fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static KNOWN: Mutex<Option<(Instant, String)>> = Mutex::new(None);
        let mut known = KNOWN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, name)) = known
            .as_ref()
            .filter(|(read, _)| read.elapsed() < Duration::from_secs(300))
        {
            return name.clone();
        }
        let name = read_mac_computer_name();
        *known = Some((Instant::now(), name.clone()));
        name
    }
    #[cfg(not(target_os = "macos"))]
    {
        ["COMPUTERNAME", "HOSTNAME"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .find(|value| !value.is_empty())
            .unwrap_or_else(|| "PC".to_string())
    }
}

#[cfg(target_os = "macos")]
fn read_mac_computer_name() -> String {
    for (program, args) in [
        ("/usr/sbin/scutil", vec!["--get", "ComputerName"]),
        ("/bin/hostname", vec![]),
    ] {
        if let Ok(out) = rightkit_child_process::run(
            program,
            args,
            &rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(5)),
        ) {
            let text = String::from_utf8_lossy(&out.stdout.bytes)
                .trim()
                .to_string();
            if out.status.success() && !out.timed_out && !text.is_empty() {
                return text;
            }
        }
    }
    "Mac".to_string()
}

/// The platform word a device announces as its model: `macOS`, `Windows` or `Computer`.
pub fn device_model() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Computer"
    }
}
