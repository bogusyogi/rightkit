//! Read and require the macOS display accessibility preferences and the scroller style for QA
//! (rightkit#435): `reduceMotion`, `reduceTransparency`, `increaseContrast`,
//! `differentiateWithoutColor` and `AppleShowScrollBars`.
//!
//! A QA run of an app whose rendering follows those preferences (reduced motion, translucency,
//! overlay or legacy scrollers) is only meaningful when the preferences are what the journey
//! assumes, so a journey asks `require`: it fails closed when a preference differs. On a
//! continuous-integration machine (`CI` or `GITHUB_ACTIONS` set) `require` may be allowed to write
//! the expected values first (`allow_write_on_ci`); on the owner's machine it never writes, because
//! the preferences are the owner's (Adrian, 2026-11-03: "my data ... snapshot and restore").
//!
//! The values are read with `defaults` from `com.apple.universalaccess` and the global domain: the
//! stored preference, which is what the system settings pane writes. `AppleShowScrollBars` of
//! `Automatic` follows the attached pointing device at run time, so `scroller_style` reports it as
//! `automatic` and does not resolve it. Reading from other platforms is an error.

use crate::process::output_bounded;
use crate::util::{err, Error, Result};
use serde_json::{json, Value};
use std::process::Command;
use std::time::Duration;

const DOMAIN: &str = "com.apple.universalaccess";
const TIMEOUT: Duration = Duration::from_secs(15);

/// The four display accessibility switches and the stored scroll-bar preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayPrefs {
    pub reduce_motion: bool,
    pub reduce_transparency: bool,
    pub increase_contrast: bool,
    pub differentiate_without_color: bool,
    /// `Automatic`, `WhenScrolling` or `Always` (`AppleShowScrollBars`; `Automatic` when unset).
    pub show_scroll_bars: String,
}

impl DisplayPrefs {
    /// `legacy` (`Always`), `overlay` (`WhenScrolling`) or `automatic` (`Automatic`, which follows the
    /// attached pointing device and is not resolved here).
    pub fn scroller_style(&self) -> &'static str {
        match self.show_scroll_bars.as_str() {
            "Always" => "legacy",
            "WhenScrolling" => "overlay",
            _ => "automatic",
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "schema": "rightkit.qa.display-prefs/1",
            "reduceMotion": self.reduce_motion,
            "reduceTransparency": self.reduce_transparency,
            "increaseContrast": self.increase_contrast,
            "differentiateWithoutColor": self.differentiate_without_color,
            "showScrollBars": self.show_scroll_bars,
            "scrollerStyle": self.scroller_style(),
        })
    }
}

/// What a journey requires; a `None` field is not checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expected {
    pub reduce_motion: Option<bool>,
    pub reduce_transparency: Option<bool>,
    pub increase_contrast: Option<bool>,
    pub differentiate_without_color: Option<bool>,
    /// `Automatic`, `WhenScrolling` or `Always`.
    pub show_scroll_bars: Option<String>,
}

impl Expected {
    /// Parse `name=value` (`reduceMotion=false`, `scrollBars=Always`). Names: `reduceMotion`,
    /// `reduceTransparency`, `increaseContrast`, `differentiateWithoutColor`, `scrollBars`.
    pub fn set(&mut self, pair: &str) -> Result<()> {
        let Some((name, value)) = pair.split_once('=') else {
            return err(format!("{pair:?} is not name=value"));
        };
        let flag = |v: &str| match v {
            "true" | "1" | "on" => Ok(true),
            "false" | "0" | "off" => Ok(false),
            other => err(format!("{name}: {other:?} is not true or false")),
        };
        match name {
            "reduceMotion" => self.reduce_motion = Some(flag(value)?),
            "reduceTransparency" => self.reduce_transparency = Some(flag(value)?),
            "increaseContrast" => self.increase_contrast = Some(flag(value)?),
            "differentiateWithoutColor" => self.differentiate_without_color = Some(flag(value)?),
            "scrollBars" => match value {
                "Automatic" | "WhenScrolling" | "Always" => {
                    self.show_scroll_bars = Some(value.to_string())
                }
                other => {
                    return err(format!(
                        "scrollBars: {other:?} is not Automatic, WhenScrolling or Always"
                    ))
                }
            },
            other => return err(format!("unknown display preference {other:?}")),
        }
        Ok(())
    }

    fn mismatches(&self, now: &DisplayPrefs) -> Vec<String> {
        let mut out = Vec::new();
        let mut check = |name: &str, want: Option<bool>, have: bool| {
            if let Some(w) = want {
                if w != have {
                    out.push(format!("{name} is {have}, expected {w}"));
                }
            }
        };
        check("reduceMotion", self.reduce_motion, now.reduce_motion);
        check(
            "reduceTransparency",
            self.reduce_transparency,
            now.reduce_transparency,
        );
        check(
            "increaseContrast",
            self.increase_contrast,
            now.increase_contrast,
        );
        check(
            "differentiateWithoutColor",
            self.differentiate_without_color,
            now.differentiate_without_color,
        );
        if let Some(w) = &self.show_scroll_bars {
            if *w != now.show_scroll_bars {
                out.push(format!(
                    "AppleShowScrollBars is {}, expected {w}",
                    now.show_scroll_bars
                ));
            }
        }
        out
    }
}

fn defaults(args: &[&str]) -> Result<Option<String>> {
    if !cfg!(target_os = "macos") {
        return err("the display accessibility preferences are read on macOS only");
    }
    let out = output_bounded(Command::new("defaults").args(args), TIMEOUT)
        .map_err(|e| Error(format!("cannot run defaults: {e}")))?
        .ok_or_else(|| Error("defaults did not answer in time".into()))?;
    if !out.status.success() {
        // An unset key is the system default: not an error.
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

fn read_flag(key: &str) -> Result<bool> {
    Ok(matches!(
        defaults(&["read", DOMAIN, key])?.as_deref(),
        Some("1") | Some("true")
    ))
}

/// The preferences as stored now.
pub fn snapshot() -> Result<DisplayPrefs> {
    let bars = defaults(&["read", "-g", "AppleShowScrollBars"])?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Automatic".to_string());
    Ok(DisplayPrefs {
        reduce_motion: read_flag("reduceMotion")?,
        reduce_transparency: read_flag("reduceTransparency")?,
        increase_contrast: read_flag("increaseContrast")?,
        differentiate_without_color: read_flag("differentiateWithoutColor")?,
        show_scroll_bars: bars,
    })
}

fn on_ci() -> bool {
    ["CI", "GITHUB_ACTIONS"].iter().any(|k| {
        std::env::var(k)
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true"))
            .unwrap_or(false)
    })
}

fn write_flag(key: &str, value: bool) -> Result<()> {
    let v = if value { "true" } else { "false" };
    defaults(&["write", DOMAIN, key, "-bool", v])?;
    Ok(())
}

/// Check the stored preferences against `expected` and return them. A preference that differs is an
/// error that names it and its value, never a silent pass. With `allow_write_on_ci` and a CI
/// environment the differing preferences are written first and read back; the owner's machine is
/// never written.
pub fn require(expected: &Expected, allow_write_on_ci: bool) -> Result<DisplayPrefs> {
    let now = snapshot()?;
    let wrong = expected.mismatches(&now);
    if wrong.is_empty() {
        return Ok(now);
    }
    if !(allow_write_on_ci && on_ci()) {
        return err(format!(
            "display preferences differ from what the journey requires: {}. Not changed: they are the owner's settings (a CI machine may allow the write with allow_write_on_ci)",
            wrong.join("; ")
        ));
    }
    let want = |v: Option<bool>, have: bool| v.unwrap_or(have);
    write_flag(
        "reduceMotion",
        want(expected.reduce_motion, now.reduce_motion),
    )?;
    write_flag(
        "reduceTransparency",
        want(expected.reduce_transparency, now.reduce_transparency),
    )?;
    write_flag(
        "increaseContrast",
        want(expected.increase_contrast, now.increase_contrast),
    )?;
    write_flag(
        "differentiateWithoutColor",
        want(
            expected.differentiate_without_color,
            now.differentiate_without_color,
        ),
    )?;
    if let Some(bars) = &expected.show_scroll_bars {
        defaults(&["write", "-g", "AppleShowScrollBars", "-string", bars])?;
    }
    let after = snapshot()?;
    let still = expected.mismatches(&after);
    if !still.is_empty() {
        return err(format!(
            "the display preferences did not take the written values: {}",
            still.join("; ")
        ));
    }
    Ok(after)
}

/// `rightkit-qa display-prefs [--json] [--require name=value]... [--allow-write-on-ci]`.
pub fn cli(args: &[String]) -> Result<i32> {
    let mut expected = Expected::default();
    let mut any = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--require" {
            let Some(pair) = args.get(i + 1) else {
                return err("--require needs name=value");
            };
            expected.set(pair)?;
            any = true;
            i += 2;
        } else {
            i += 1;
        }
    }
    let allow = args.iter().any(|a| a == "--allow-write-on-ci");
    let prefs = if any {
        match require(&expected, allow) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("rightkit-qa display-prefs: {e}");
                return Ok(1);
            }
        }
    } else {
        snapshot()?
    };
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&prefs.to_json()).unwrap_or_default()
        );
    } else {
        println!(
            "reduceMotion={} reduceTransparency={} increaseContrast={} differentiateWithoutColor={} scrollBars={} ({})",
            prefs.reduce_motion,
            prefs.reduce_transparency,
            prefs.increase_contrast,
            prefs.differentiate_without_color,
            prefs.show_scroll_bars,
            prefs.scroller_style()
        );
    }
    Ok(0)
}
