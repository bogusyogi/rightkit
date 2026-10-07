//! Sign-ins other tools on this machine already hold: the GitHub CLI and Git's credential
//! helper (Git Credential Manager on Windows/macOS). Read-only, never interactive: each
//! child runs hidden (no console window), kill-on-close, under a timeout, with every prompt
//! path disabled. A tool that is missing, signed out, or slow simply yields nothing.

use std::io::{Read, Write};
use std::process::Stdio;
use std::time::Duration;

use rightkit_http::Secret;
use rightkit_process::OwnedCommand;

/// Default bound on one system-source lookup.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// A credential source outside this app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSource {
    /// `gh auth token --hostname <host>`.
    GitHubCli { host: String },
    /// `git credential fill` for `https://<host>`.
    GitCredential { host: String },
}

impl SystemSource {
    /// GitHub CLI, then Git's credential helper, for github.com.
    pub fn github() -> Vec<SystemSource> {
        vec![
            SystemSource::GitHubCli {
                host: "github.com".into(),
            },
            SystemSource::GitCredential {
                host: "github.com".into(),
            },
        ]
    }

    /// Look the credential up, or `None` when this source has nothing usable.
    pub fn lookup(&self, timeout: Duration) -> Option<Secret> {
        match self {
            SystemSource::GitHubCli { host } => {
                let out = run(
                    "gh",
                    &["auth", "token", "--hostname", host],
                    &[("GH_PROMPT_DISABLED", "1"), ("NO_COLOR", "1")],
                    None,
                    timeout,
                )?;
                let value = out.trim();
                (!value.is_empty() && !value.contains(char::is_whitespace))
                    .then(|| Secret::new(value))
            }
            SystemSource::GitCredential { host } => {
                let request = format!("protocol=https\nhost={host}\n\n");
                let out = run(
                    "git",
                    &[
                        "-c",
                        "credential.interactive=never",
                        "-c",
                        "core.askPass=",
                        "credential",
                        "fill",
                    ],
                    &[
                        ("GIT_TERMINAL_PROMPT", "0"),
                        ("GCM_INTERACTIVE", "never"),
                        ("GIT_ASKPASS", ""),
                        ("SSH_ASKPASS", ""),
                    ],
                    Some(request.as_bytes()),
                    timeout,
                )?;
                out.lines()
                    .find_map(|line| line.strip_prefix("password="))
                    .filter(|v| !v.is_empty())
                    .map(Secret::new)
            }
        }
    }
}

/// Run a helper hidden and bounded; `None` on any failure. Output is never logged.
fn run(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Option<String> {
    let mut command = OwnedCommand::new(program);
    command
        .command_mut()
        .args(args)
        .envs(env.iter().copied())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.take_stdin()) {
        let _ = pipe.write_all(bytes);
    }
    let mut stdout = child.take_stdout()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => reader.join().ok(),
        _ => {
            let _ = child.terminate_tree();
            None
        }
    }
}
