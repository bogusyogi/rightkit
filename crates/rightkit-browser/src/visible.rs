//! A visible browser window for a step a person completes themselves (a sign-in), run without
//! the DevTools protocol: no remote-debugging endpoint, no session, no page control. The caller
//! opens it, waits until the person closes it, and reads how it exited. Use a [`BrowserSession`]
//! for anything that automates the page.
//!
//! [`BrowserSession`]: crate::BrowserSession

use crate::config::{
    chrome_for_testing_path, reject_real_browser_profile, validate_extra_args, validate_profile_dir,
};
use crate::error::{BrowserError, Result};
use rightkit_child_process::OwnedCommand;
use std::path::PathBuf;
use std::process::Stdio;

/// What to open in the visible browser.
#[derive(Clone, Debug)]
pub struct VisibleLaunch {
    /// Absolute `http` or `https` URL to open first.
    pub url: String,
    /// Existing absolute directory the browser keeps its profile in. The caller owns it and must
    /// not point it at a real browser's own user-data directory (refused).
    pub profile_dir: PathBuf,
    /// Browser executable. `None` uses Chrome for Testing ([`chrome_for_testing_path`]).
    pub executable: Option<PathBuf>,
    /// Extra Chrome flags. Flags the session manages (profile, debugging endpoint) are refused.
    pub args: Vec<String>,
}

/// How the visible browser exited once the person closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleExit {
    /// `true` when the process exited with status zero.
    pub success: bool,
    /// The exit code; `None` when the process ended by a signal.
    pub code: Option<i32>,
}

/// Start the browser showing `launch.url` and block until its process exits. No remote
/// debugging flag is passed, so the browser exposes no control endpoint. Blocking: call it from
/// a dedicated thread or `spawn_blocking`.
///
/// Chrome hands a request to an already running instance on the same profile and then exits at
/// once, so the caller must give each sign-in its own profile directory.
pub fn run_visible_until_exit(launch: &VisibleLaunch) -> Result<VisibleExit> {
    validate_profile_dir(&launch.profile_dir)?;
    reject_real_browser_profile(&launch.profile_dir)?;
    validate_extra_args(&launch.args)?;
    let url = url::Url::parse(&launch.url)
        .map_err(|e| BrowserError::Invalid(format!("visible browser URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BrowserError::Invalid(
            "visible browser URL must be http or https".into(),
        ));
    }
    let executable = match &launch.executable {
        Some(path) => path.clone(),
        None => chrome_for_testing_path(None)?,
    };
    let mut cmd = OwnedCommand::new(&executable);
    cmd.command_mut()
        .arg(format!("--user-data-dir={}", launch.profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .args(&launch.args)
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    let status = child
        .wait()
        .map_err(|e| BrowserError::Launch(format!("waiting for the visible browser: {e}")))?;
    Ok(VisibleExit {
        success: status.success(),
        code: status.code(),
    })
}
