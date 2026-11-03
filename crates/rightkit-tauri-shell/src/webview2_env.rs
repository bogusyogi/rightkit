//! WebView2 profile environment: the user-data folder and extra browser arguments an app gives its
//! WebView2 engine (Orthic-Labs/rightkit#386).
//!
//! WebView2 reads `WEBVIEW2_USER_DATA_FOLDER`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` and
//! `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` from the process environment, and they win over the API
//! parameters the app passes. So [`apply`] must run during startup, before any window exists; a later
//! call changes nothing for an environment that already exists. Everything here is pure except
//! [`apply`], which sets the variables for this process only. On Windows it first scrubs an inherited
//! value of those loader variables that the profile does not set (an inherited browser-arguments value
//! could open a remote-debugging port, an inherited executable folder could load another browser), in
//! every build; no Cargo feature keeps inherited values. A profile that calls
//! [`ProfileEnv::keep_existing_user_data_folder`] keeps a launcher-chosen user data folder.
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
/// Environment variable WebView2 reads for the folder that holds the browser it loads.
pub const ENV_BROWSER_EXECUTABLE_FOLDER: &str = "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER";
/// Environment variable WebView2 reads for the first-paint background, `AARRGGBB` hex.
pub const ENV_DEFAULT_BACKGROUND_COLOR: &str = "WEBVIEW2_DEFAULT_BACKGROUND_COLOR";
/// The loader variables an inherited value of which the shell removes on Windows before it applies
/// the profile.
const LOADER_VARIABLES: [&str; 3] = [
    ENV_USER_DATA_FOLDER,
    ENV_ADDITIONAL_BROWSER_ARGUMENTS,
    ENV_BROWSER_EXECUTABLE_FOLDER,
];
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
    /// First-paint background `[r, g, b, a]`. `None` leaves WebView2's default (white).
    pub background_color: Option<[u8; 4]>,
    /// Keep a user data folder that is already set in the environment (a QA launcher chose it)
    /// instead of replacing it with this profile's folder.
    pub keep_existing_user_data_folder: bool,
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

    /// Paint the first frame in `color` (`[r, g, b, a]`), so a hidden window does not paint white
    /// before the page loads. `None` keeps WebView2's default.
    pub fn default_background_color(mut self, color: Option<[u8; 4]>) -> Self {
        self.background_color = color;
        self
    }

    /// Keep a user data folder already set in the environment instead of this profile's folder.
    pub fn keep_existing_user_data_folder(mut self, keep: bool) -> Self {
        self.keep_existing_user_data_folder = keep;
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
    if let Some([r, g, b, a]) = env.background_color {
        out.push((
            ENV_DEFAULT_BACKGROUND_COLOR,
            format!("{a:02X}{r:02X}{g:02X}{b:02X}"),
        ));
    }
    Ok(out)
}

/// Validate `env`, create its user data folder, then set the WebView2 variables for this process.
/// Call during startup, before any thread reads the environment (the variables are process-wide).
/// Validation runs for the whole profile before anything is set or created. On Windows an inherited
/// loader variable the profile does not set is removed (see the module documentation).
pub fn apply(env: &ProfileEnv) -> Result<(), ShellError> {
    let mut entries = plan(env)?;
    let inherited_folder = std::env::var_os(ENV_USER_DATA_FOLDER).filter(|v| !v.is_empty());
    let keep_folder = env.keep_existing_user_data_folder && inherited_folder.is_some();
    if keep_folder {
        entries.retain(|(name, _)| *name != ENV_USER_DATA_FOLDER);
    }
    if let Some(folder) = &env.user_data_folder {
        if !keep_folder {
            std::fs::create_dir_all(folder).map_err(|e| {
                ShellError::Io(format!("create user data folder {}: {e}", folder.display()))
            })?;
        }
    }
    if cfg!(windows) {
        for name in LOADER_VARIABLES {
            let kept = entries.iter().any(|(set, _)| *set == name)
                || (keep_folder && name == ENV_USER_DATA_FOLDER);
            if !kept {
                std::env::remove_var(name);
            }
        }
    }
    for (name, value) in entries {
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
