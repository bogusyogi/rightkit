//! WebView2 profile environment: the user-data folder and extra browser arguments an app gives its
//! WebView2 engine (Orthic-Labs/rightkit#386).
//!
//! WebView2 reads `WEBVIEW2_USER_DATA_FOLDER` and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` from the
//! process environment when it creates its environment, the first time a webview is made. So
//! [`apply`] must run during startup, before any window exists; a later call changes nothing for an
//! environment that already exists. Everything here is pure except [`apply`], which sets the two
//! variables for this process only and never removes an existing value.
//!
//! Donor: ViewRight's profile environment (`lib.rs:178-208` at its HEAD). That source is not in this
//! checkout, so the surface here is the shell's own: [`ProfileEnv`] with a validated, typed view of
//! the two variables. Arguments are validated because WebView2 splits them on spaces: an argument
//! with whitespace, a control character or no leading `--` is refused, and so is any argument that
//! opens a debugging port, turns off the sandbox or lifts web security. Those are shell policy, not
//! app choice.

use std::path::{Component, Path, PathBuf};

use crate::ShellError;

/// Environment variable WebView2 reads for its user-data folder.
pub const ENV_USER_DATA_FOLDER: &str = "WEBVIEW2_USER_DATA_FOLDER";
/// Environment variable WebView2 reads for extra Chromium arguments (space separated).
pub const ENV_ADDITIONAL_BROWSER_ARGUMENTS: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
/// Most extra arguments one profile may carry.
pub const MAX_ARGUMENTS: usize = 16;
/// Longest accepted extra argument, in bytes.
pub const MAX_ARGUMENT_BYTES: usize = 256;

/// Argument prefixes the shell refuses (matched case-insensitively, after the leading `--`).
const REFUSED_ARGUMENT_PREFIXES: [&str; 5] = [
    "--remote-debugging",
    "--remote-allow-origins",
    "--no-sandbox",
    "--disable-web-security",
    "--allow-file-access-from-files",
];

/// The WebView2 profile an app asks for. Build it with [`ProfileEnv::new`] and the setters, then
/// hand it to [`apply`] before the first webview is created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileEnv {
    /// Absolute folder that holds the WebView2 profile. `None` leaves WebView2's default.
    pub user_data_folder: Option<PathBuf>,
    /// Extra Chromium arguments, each starting with `--`.
    pub additional_browser_args: Vec<String>,
}

impl ProfileEnv {
    /// An empty profile: WebView2 defaults, no extra arguments.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the user-data folder (must be absolute; checked by [`plan`]).
    pub fn user_data_folder(mut self, folder: impl Into<PathBuf>) -> Self {
        self.user_data_folder = Some(folder.into());
        self
    }

    /// Add one extra browser argument (checked by [`plan`]).
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.additional_browser_args.push(argument.into());
        self
    }
}

/// The `(variable, value)` pairs [`apply`] would set, after validation. Pure: nothing is set.
pub fn plan(env: &ProfileEnv) -> Result<Vec<(&'static str, String)>, ShellError> {
    let mut out = Vec::new();
    if let Some(folder) = &env.user_data_folder {
        out.push((
            ENV_USER_DATA_FOLDER,
            checked_folder(folder)?.to_string_lossy().into_owned(),
        ));
    }
    if !env.additional_browser_args.is_empty() {
        out.push((
            ENV_ADDITIONAL_BROWSER_ARGUMENTS,
            checked_arguments(&env.additional_browser_args)?,
        ));
    }
    Ok(out)
}

/// Validate `env` and set the WebView2 variables for this process. Call during startup, before any
/// thread reads the environment (the variables are process-wide). A value the profile does not
/// set is left alone. Validation runs for the whole profile before anything is set.
pub fn apply(env: &ProfileEnv) -> Result<(), ShellError> {
    for (name, value) in plan(env)? {
        std::env::set_var(name, value);
    }
    Ok(())
}

fn checked_folder(folder: &Path) -> Result<&Path, ShellError> {
    let text = folder.to_string_lossy();
    if text.trim().is_empty() || text.contains('\0') {
        return Err(ShellError::Invalid("user data folder is empty".into()));
    }
    if !folder.is_absolute() {
        return Err(ShellError::Invalid(
            "user data folder must be an absolute path".into(),
        ));
    }
    if folder
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ShellError::Invalid(
            "user data folder must not contain '..'".into(),
        ));
    }
    Ok(folder)
}

fn checked_arguments(arguments: &[String]) -> Result<String, ShellError> {
    if arguments.len() > MAX_ARGUMENTS {
        return Err(ShellError::Invalid(format!(
            "at most {MAX_ARGUMENTS} browser arguments"
        )));
    }
    for argument in arguments {
        check_argument(argument)?;
    }
    Ok(arguments.join(" "))
}

fn check_argument(argument: &str) -> Result<(), ShellError> {
    let refuse = |why: &str| ShellError::Invalid(format!("browser argument refused: {why}"));
    if argument.len() > MAX_ARGUMENT_BYTES {
        return Err(refuse("too long"));
    }
    if !argument.starts_with("--") || argument.len() == 2 {
        return Err(refuse("must start with a double dash"));
    }
    if argument
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '"')
    {
        return Err(refuse(
            "contains whitespace, a control character or a quote",
        ));
    }
    let lower = argument.to_ascii_lowercase();
    if REFUSED_ARGUMENT_PREFIXES
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Err(refuse("not allowed by shell policy"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\ProgramData\App\WebView2")
        } else {
            PathBuf::from("/var/tmp/app/webview2")
        }
    }

    #[test]
    fn empty_profile_plans_nothing() {
        assert!(plan(&ProfileEnv::new()).unwrap().is_empty());
    }

    #[test]
    fn folder_and_arguments_plan_in_fixed_order() {
        let env = ProfileEnv::new()
            .user_data_folder(absolute())
            .arg("--disable-features=Translate")
            .arg("--lang=en-GB");
        let planned = plan(&env).unwrap();
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].0, ENV_USER_DATA_FOLDER);
        assert_eq!(planned[0].1, absolute().to_string_lossy());
        assert_eq!(planned[1].0, ENV_ADDITIONAL_BROWSER_ARGUMENTS);
        assert_eq!(planned[1].1, "--disable-features=Translate --lang=en-GB");
    }

    #[test]
    fn relative_or_parent_folders_are_refused() {
        assert!(plan(&ProfileEnv::new().user_data_folder("relative/dir")).is_err());
        let up = if cfg!(windows) {
            r"C:\ProgramData\..\Other"
        } else {
            "/var/tmp/../etc"
        };
        assert!(plan(&ProfileEnv::new().user_data_folder(up)).is_err());
        assert!(plan(&ProfileEnv::new().user_data_folder("")).is_err());
    }

    #[test]
    fn arguments_must_be_single_flags() {
        for bad in [
            "lang=en",
            "--",
            "--lang=en GB",
            "--a\tb",
            "--x\"y",
            "--remote-debugging-port=9222",
            "--REMOTE-DEBUGGING-PIPE",
            "--no-sandbox",
            "--disable-web-security",
        ] {
            assert!(
                plan(&ProfileEnv::new().arg(bad)).is_err(),
                "should refuse {bad:?}"
            );
        }
        let long = format!("--{}", "a".repeat(MAX_ARGUMENT_BYTES));
        assert!(plan(&ProfileEnv::new().arg(long)).is_err());
    }

    #[test]
    fn too_many_arguments_are_refused() {
        let mut env = ProfileEnv::new();
        for i in 0..=MAX_ARGUMENTS {
            env = env.arg(format!("--flag-{i}"));
        }
        assert!(plan(&env).is_err());
    }
}
