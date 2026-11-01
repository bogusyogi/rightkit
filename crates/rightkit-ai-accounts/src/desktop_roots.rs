//! Claude Desktop data folders on this computer: the default folder, each account-switcher slot
//! The host app keeps under its state folder, and each second profile under the account's
//! secondary-profiles folder.

use std::path::{Path, PathBuf};

/// A profile folder's name as people say it: `claude-b-home` is "Claude B".
pub fn profile_label(folder: &str) -> String {
    let base = folder.strip_suffix("-home").unwrap_or(folder);
    base.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Claude Desktop's data folders that can hold Claude Code session records: its default folder
/// (`~/Library/Application Support/Claude` on a Mac, `%APPDATA%\Claude` on Windows, no label) and
/// each account-switcher slot's own folder under the host app's state (`.../the host app/claude-desktop/<slot>`,
/// labelled with the slot's name). The same places `claude_desktop.rs` starts Desktop with.
pub fn desktop_slot_roots(account: &Path) -> Vec<(PathBuf, Option<String>)> {
    #[cfg(target_os = "macos")]
    let (main, slots) = {
        let support = account.join("Library").join("Application Support");
        (
            Some(support.join("Claude")),
            rightkit_app_host::state_root().map(|root| root.join("claude-desktop")),
        )
    };
    #[cfg(windows)]
    let (main, slots) = {
        let _ = account;
        // `home::env_var`, not the raw variables: from a Claude B shell they point into
        // ClaudeProfiles, which would list B's folder as Desktop's own (#420, #368).
        (
            rightkit_app_host::env_var("APPDATA").map(|a| PathBuf::from(a).join("Claude")),
            rightkit_app_host::state_root().map(|root| root.join("claude-desktop")),
        )
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let (main, slots): (Option<PathBuf>, Option<PathBuf>) = {
        let _ = account;
        (None, None)
    };
    let mut roots: Vec<(PathBuf, Option<String>)> = Vec::new();
    roots.extend(main.map(|m| (m, None)));
    if let Some(listing) = slots.and_then(|s| std::fs::read_dir(s).ok()) {
        let mut found: Vec<PathBuf> = listing
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        found.sort();
        for dir in found {
            let label = dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(profile_label)
                .filter(|l| !l.is_empty());
            roots.push((dir, label));
        }
    }
    roots
}

/// Every Claude Desktop data folder that can hold Code chat records (#410), as
/// `(folder, profile label, is Desktop's default folder)`: the default folder, each
/// account-switcher slot's, and the data folder of each second profile under `ClaudeProfiles`
/// (found by scanning: `<profile>/Library/Application Support/Claude` on a Mac,
/// `<profile>/AppData/Roaming/Claude` on Windows, with the profile's own `home` folder tried
/// too). Only the default folder's Desktop takes the `claude://` link a wake uses.
pub fn desktop_chat_roots() -> Vec<(PathBuf, Option<String>, bool)> {
    let account = rightkit_app_host::account_home();
    let mut out: Vec<(PathBuf, Option<String>, bool)> = desktop_slot_roots(&account)
        .into_iter()
        .map(|(root, label)| {
            let main = label.is_none();
            (root, label, main)
        })
        .collect();
    if let Ok(listing) = std::fs::read_dir(account.join("ClaudeProfiles")) {
        let mut profiles: Vec<PathBuf> = listing.flatten().map(|e| e.path()).collect();
        profiles.sort();
        for profile in profiles.into_iter().filter(|p| p.is_dir()) {
            let label = profile
                .file_name()
                .and_then(|n| n.to_str())
                .map(profile_label)
                .filter(|l| !l.is_empty());
            for base in [profile.clone(), profile.join("home")] {
                for data in [
                    base.join("Library")
                        .join("Application Support")
                        .join("Claude"),
                    base.join("AppData").join("Roaming").join("Claude"),
                ] {
                    if data.join("claude-code-sessions").is_dir()
                        && !out.iter().any(|(r, _, _)| *r == data)
                    {
                        out.push((data, label.clone(), false));
                    }
                }
            }
        }
    }
    out
}
