//! The user's login-shell PATH and selected environment values, for an app launched from the
//! desktop (Finder, the Dock, a launch agent) that inherits only the session's minimal
//! environment. Unix only: on Windows every function returns nothing and runs nothing.
//!
//! The shell is `$SHELL` when it is an absolute, existing file, else `/bin/zsh`, else `/bin/sh`.
//! It runs as `shell -lic <script>` (login and interactive, so the profile files are read) under
//! [`run`](crate::run) with stdin closed, a hard deadline (the whole tree is killed at the
//! deadline, so a profile that waits for a terminal cannot hang the caller) and a capped output.
//! A timeout, a failure to start, truncated output, or a missing marker returns nothing.
//!
//! Environment values are secrets. They are returned to the caller only, never printed, logged
//! or placed on a command line (the names are checked to be plain identifiers before they go
//! into the script).

use std::time::Duration;

#[cfg(unix)]
use crate::{run, RunOptions};

#[cfg(unix)]
const PATH_BEGIN: &str = "__RIGHTKIT_PATH_BEGIN__";
#[cfg(unix)]
const PATH_END: &str = "__RIGHTKIT_PATH_END__";
#[cfg(unix)]
const ENV_BEGIN: &str = "__RIGHTKIT_ENV_BEGIN__";
#[cfg(unix)]
const ENV_END: &str = "__RIGHTKIT_ENV_END__";
/// Most bytes read from the shell's output; more than this is treated as incomplete.
#[cfg(unix)]
const OUTPUT_CAP: usize = 256 * 1024;

/// The login shell's `PATH`, or `None` when it cannot be read within `timeout`.
pub fn path(timeout: Duration) -> Option<String> {
    #[cfg(unix)]
    {
        let script =
            format!("printf '%s' '{PATH_BEGIN}'; printf '%s' \"$PATH\"; printf '%s' '{PATH_END}'");
        let output = run(shell(), ["-lic", script.as_str()], &capped(timeout)).ok()?;
        if output.timed_out || output.stdout.truncated {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout.bytes);
        let (_, rest) = text.split_once(PATH_BEGIN)?;
        let (value, _) = rest.split_once(PATH_END)?;
        (!value.is_empty()).then(|| value.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

/// The login shell's values of the named environment variables, as `(name, value)` pairs for the
/// names that are set and non-empty there. Names that are not plain identifiers are skipped, and
/// nothing is returned when the shell cannot answer within `timeout`. The values are secrets:
/// the caller must not log them.
pub fn env_values(names: &[String], timeout: Duration) -> Vec<(String, String)> {
    #[cfg(unix)]
    {
        let wanted: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|name| is_identifier(name))
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        let mut script = format!("printf '%s' '{ENV_BEGIN}'");
        for name in &wanted {
            // Each value is followed by a NUL, so an empty value keeps its place in the list.
            script.push_str(&format!("; printf '%s\\0' \"${{{name}-}}\""));
        }
        script.push_str(&format!("; printf '%s' '{ENV_END}'"));
        let Ok(output) = run(shell(), ["-lic", script.as_str()], &capped(timeout)) else {
            return Vec::new();
        };
        if output.timed_out || output.stdout.truncated {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&output.stdout.bytes);
        let Some((_, rest)) = text.split_once(ENV_BEGIN) else {
            return Vec::new();
        };
        let Some((body, _)) = rest.split_once(ENV_END) else {
            return Vec::new();
        };
        body.split('\0')
            .zip(wanted)
            .filter(|(value, _)| !value.is_empty())
            .map(|(value, name)| (name.to_string(), value.to_string()))
            .collect()
    }
    #[cfg(not(unix))]
    {
        let _ = (names, timeout);
        Vec::new()
    }
}

#[cfg(unix)]
fn capped(timeout: Duration) -> RunOptions {
    RunOptions::new(timeout)
        .stdout_cap(OUTPUT_CAP)
        .stderr_cap(0)
        .no_window(true)
}

/// `$SHELL` when it is an absolute path to an existing file with no control characters, else
/// `/bin/zsh`, else `/bin/sh`.
#[cfg(unix)]
fn shell() -> std::path::PathBuf {
    use std::path::PathBuf;
    let from_env = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| {
            shell.is_absolute()
                && shell.is_file()
                && shell
                    .to_str()
                    .is_some_and(|text| !text.chars().any(char::is_control))
        });
    from_env.unwrap_or_else(|| {
        ["/bin/zsh", "/bin/sh"]
            .iter()
            .map(PathBuf::from)
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
    })
}

#[cfg(unix)]
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_plain_identifiers_reach_the_script() {
        assert!(is_identifier("PATH"));
        assert!(is_identifier("_X1"));
        assert!(!is_identifier("1X"));
        assert!(!is_identifier("A;rm"));
        assert!(!is_identifier("A B"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn a_nonexistent_shell_falls_back_to_a_real_one() {
        let shell = shell();
        assert!(shell.is_absolute());
        assert!(shell.is_file());
    }

    #[test]
    fn env_values_skips_invalid_names_and_returns_nothing_for_none_valid() {
        assert!(env_values(&["not valid".to_string()], Duration::from_secs(1)).is_empty());
    }
}
