//! Claude command-line accounts: one `CLAUDE_CONFIG_DIR` folder per account under the host app's
//! state folder, and a choice of which one new terminal sessions use. See
//! docs/claude-account-switch.md ("Command-line accounts").
//!
//! The host app never reads or writes a login: the sign-in happens in `claude`'s own flow inside the
//! account's folder. Switching writes only `active`, `env.sh` and `env.ps1`; a running session
//! keeps the folder it started with because an environment variable cannot be changed from
//! outside a process.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Longest account name.
const NAME_MAX: usize = 32;

/// The folder that holds everything here: `<the host app state folder>/claude-cli`.
pub fn default_base() -> io::Result<PathBuf> {
    #[cfg(any(target_os = "macos", windows))]
    let base = rightkit_app_host::state_root();
    #[cfg(not(any(target_os = "macos", windows)))]
    let base: Option<PathBuf> = None;
    base.map(|b| b.join("claude-cli"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no known location"))
}

/// An account name is a short file-name-safe word: letters, digits, `.`, `_`, `-`, not
/// starting with `.` or `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[derive(Debug, Clone, Serialize)]
pub struct CliAccount {
    pub name: String,
    /// The `CLAUDE_CONFIG_DIR` folder.
    pub dir: String,
    /// True once `claude` has written its own state there (`.claude.json` exists); false means
    /// run `claude` with this account active and sign in. Existence only, nothing is read.
    pub started: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliAccounts {
    pub base: String,
    pub active: Option<String>,
    pub accounts: Vec<CliAccount>,
    /// The line that makes new terminals follow the choice, per shell.
    pub posix_line: String,
    pub powershell_line: String,
}

fn accounts_dir(base: &Path) -> PathBuf {
    base.join("accounts")
}

fn quote_posix(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn quote_powershell(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// The active account's name, when one is set and its folder exists.
pub fn active_name(base: &Path) -> Option<String> {
    let text = fs::read_to_string(base.join("active")).ok()?;
    let name = text.trim();
    (valid_name(name) && accounts_dir(base).join(name).is_dir()).then(|| name.to_string())
}

/// Every account folder with the active mark, sorted by name.
pub fn list(base: &Path) -> CliAccounts {
    let active = active_name(base);
    let mut accounts: Vec<CliAccount> = fs::read_dir(accounts_dir(base))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            // A real folder only: a symlink or file named like an account is not one.
            let kind = entry.file_type().ok()?;
            if !valid_name(&name) || !kind.is_dir() {
                return None;
            }
            let dir = entry.path();
            Some(CliAccount {
                started: dir.join(".claude.json").exists(),
                active: active.as_deref() == Some(name.as_str()),
                dir: dir.to_string_lossy().into_owned(),
                name,
            })
        })
        .collect();
    accounts.sort_by(|a, b| a.name.cmp(&b.name));
    CliAccounts {
        base: base.to_string_lossy().into_owned(),
        active,
        accounts,
        posix_line: format!(". {}", quote_posix(&base.join("env.sh"))),
        powershell_line: format!(". {}", quote_powershell(&base.join("env.ps1"))),
    }
}

/// Make `name` the account new terminal sessions use, creating its folder on first use.
/// Rewrites `active`, `env.sh` and `env.ps1`; touches no process and no other file.
pub fn use_account(base: &Path, name: &str) -> io::Result<CliAccounts> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "account name {name:?} must be 1 to {NAME_MAX} letters, digits, '.', '_' or '-', and not start with '.' or '-'"
            ),
        ));
    }
    let dir = accounts_dir(base).join(name);
    rightkit_fs::ensure_private_dir(&dir).map_err(io::Error::from)?;
    // Owner-only from creation on Unix; Windows keeps the parent's inherited access.
    let options = rightkit_fs::AtomicWriteOptions {
        privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
        ..Default::default()
    };
    let write = |file_name: &str, text: String| -> io::Result<()> {
        rightkit_fs::atomic_write_with_options(&base.join(file_name), options, |file| {
            io::Write::write_all(file, text.as_bytes())
        })
        .map_err(io::Error::from)
    };
    write(
        "env.sh",
        format!(
            "# Written by `host claude use`. Source it from your shell profile.\nexport CLAUDE_CONFIG_DIR={}\n",
            quote_posix(&dir)
        ),
    )?;
    write(
        "env.ps1",
        format!(
            "# Written by `host claude use`. Dot-source it from your PowerShell profile.\n$env:CLAUDE_CONFIG_DIR = {}\n",
            quote_powershell(&dir)
        ),
    )?;
    // The choice goes last so a reader never sees an active name with stale snippets.
    write("active", format!("{name}\n"))?;
    Ok(list(base))
}
