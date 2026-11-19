//! `--shot` option grammar for a QA launch: argument parsing and validation.
//!
//! Mechanism only. The host passes its window table, its default window and its default delay
//! in a `ShotHost`; the grammar itself names no window, title, file or delay of any product.

use std::{ffi::OsString, path::PathBuf, time::Duration};

/// One window the host lets `--shot-window` name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShotWindowSpec {
    /// The name on the command line, for example `hub`.
    pub label: &'static str,
    /// Window titles to match on Windows, tried in order; the first title that matches is captured.
    /// Must not be empty.
    pub titles: &'static [&'static str],
    /// File name of the default screenshot, placed in the working directory.
    pub default_file: &'static str,
}

/// The host's values for the `--shot` grammar.
#[derive(Clone, Copy, Debug)]
pub struct ShotHost {
    /// The windows `--shot-window` accepts, in the order their labels are listed in messages.
    pub windows: &'static [ShotWindowSpec],
    /// The window captured when `--shot-window` is absent. Should be one of `windows`.
    pub default_window: ShotWindowSpec,
    /// The wait after the window is prepared and before the capture, when `--shot-delay-ms` is absent.
    pub default_delay: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShotOptions {
    pub path: PathBuf,
    pub window: ShotWindowSpec,
    pub delay: Duration,
    pub foreground: bool,
}

impl ShotOptions {
    pub fn window_label(&self) -> &'static str {
        self.window.label
    }
}

pub fn options_from_env(host: &ShotHost) -> Result<Option<ShotOptions>, String> {
    parse_args(host, std::env::args_os().skip(1))
}

pub fn parse_args<I, T>(host: &ShotHost, args: I) -> Result<Option<ShotOptions>, String>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut path = None;
    let mut shot_requested = false;
    let mut window = host.default_window;
    let mut delay = host.default_delay;
    let mut foreground = false;
    let mut iter = args.into_iter().map(Into::into).peekable();

    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy();
        if arg == "--shot" {
            shot_requested = true;
            path = Some(match iter.peek() {
                Some(next) if !next.to_string_lossy().starts_with("--") => {
                    PathBuf::from(iter.next().unwrap())
                }
                _ => continue,
            });
        } else if let Some(value) = arg.strip_prefix("--shot=") {
            shot_requested = true;
            path = Some(PathBuf::from(value));
        } else if arg == "--shot-window" {
            let Some(value) = iter.next() else {
                return Err(format!("--shot-window requires {}", window_labels(host)));
            };
            window = parse_window(host, &value.to_string_lossy())?;
        } else if let Some(value) = arg.strip_prefix("--shot-window=") {
            window = parse_window(host, value)?;
        } else if arg == "--shot-delay-ms" {
            let Some(value) = iter.next() else {
                return Err("--shot-delay-ms requires a number".to_string());
            };
            delay = parse_delay(&value.to_string_lossy())?;
        } else if let Some(value) = arg.strip_prefix("--shot-delay-ms=") {
            delay = parse_delay(value)?;
        } else if arg == "--shot-foreground" {
            foreground = true;
        }
    }

    if !shot_requested {
        return Ok(None);
    }

    Ok(Some(ShotOptions {
        path: path.unwrap_or_else(|| default_path(&window)),
        window,
        delay,
        foreground,
    }))
}

/// The labels of the host's windows, joined for an error message: `hub or pill`.
fn window_labels(host: &ShotHost) -> String {
    host.windows
        .iter()
        .map(|window| window.label)
        .collect::<Vec<_>>()
        .join(" or ")
}

pub fn parse_window(host: &ShotHost, value: &str) -> Result<ShotWindowSpec, String> {
    host.windows
        .iter()
        .copied()
        .find(|window| window.label == value)
        .ok_or_else(|| {
            format!(
                "unsupported --shot-window value {value:?}; use {}",
                window_labels(host)
            )
        })
}

pub fn parse_delay(value: &str) -> Result<Duration, String> {
    let millis = value
        .parse::<u64>()
        .map_err(|_| format!("invalid --shot-delay-ms value {value:?}"))?;
    Ok(Duration::from_millis(millis))
}

pub fn default_path(window: &ShotWindowSpec) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(window.default_file)
}
