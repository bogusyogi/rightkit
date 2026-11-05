//! Memory modes of the local image model server (`sd-server`).
//!
//! Problem: profile `qwen21-base-q6` peaks at about 10.3 GB resident (measured by the coordinator
//! at 512x512 / 20 steps; weights: transformer 6.0 GB, text encoder 5.0 GB, vision projector
//! 1.2 GB, VAE 0.7 GB). That swaps a 24 GB Mac and does not run on a 16 GB one.
//!
//! A mode is a fixed list of real `sd-server` options (names verbatim from the pinned server's
//! `--help`, upstream tag `master-929-3f8527a`, see `data/local-image/runtime-manifest.json`):
//!
//! | mode          | options added to the profile's arguments                                  |
//! |---------------|----------------------------------------------------------------------------|
//! | `performance` | none (today's exact arguments; every weight stays resident)                |
//! | `balanced`    | `--params-backend te=disk` `--vae-tiling`                                  |
//! | `low`         | `--params-backend disk` `--max-vram 4` `--disable-prefetch` `--vae-tiling` |
//!
//! What each option does (from the help text):
//! - `--params-backend <module>=disk`: the module's weights are kept on disk and brought in
//!   segment by segment, instead of staying resident. Output is identical; it is slower, because
//!   weights are re-read every job. An explicit `--params-backend` turns the server's
//!   `--auto-fit` placement off. `te` is the text encoder (the Qwen3-VL language model and its
//!   vision projector).
//! - `--max-vram 4`: a 4 GiB budget for weights plus working buffers during segmented
//!   ("graph-cut") execution, so a large module runs in pieces. On Apple Silicon this is the same
//!   unified memory the process lives in, so it is what bounds the resident weights in `low`.
//! - `--disable-prefetch`: no asynchronous loading of the next segment while the current one runs
//!   (keeps one segment resident instead of two; slower).
//! - `--vae-tiling`: the VAE encodes and decodes in tiles (default 256x256 pixels). Lowers the
//!   decode buffer; it can change the image slightly (faint tile seams), unlike the others.
//!
//! Not used on purpose: `--offload-to-cpu` ("place the weights in RAM to save VRAM": no saving on
//! unified memory), `--clip-on-cpu` / `--vae-on-cpu` (deprecated aliases of `--backend te=cpu`,
//! same RAM), `--mmap` (behaviour with this loader unproven), `--diffusion-fa` (affects speed and
//! numerics, not residency).
//!
//! `auto` (the default) picks a mode from the machine's total memory, read from the same
//! `rightkit-models` hardware probe the model manager uses (`<PREFIX>_HW_JSON` overrides it).
//!
//! Every number here that is not marked measured is an ESTIMATE and has not been measured.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

/// Environment override (tests, CLI). Wins over the saved setting.
pub const ENV: &str = "LOCAL_IMAGE_MEMORY_MODE";

/// `auto` rule thresholds, in GiB of total physical memory as `rightkit-models` reports it
/// (rounded to 0.1; a "16 GB" Mac reports 16.0, a "16 GB" Windows PC about 15.8, so the
/// boundaries sit half a GiB above the nominal size).
///
/// - at or below `AUTO_LOW_MAX_GB`: `low`. Performance peaks at about 10.3 GB (measured) and a
///   16 GB Mac leaves roughly 10 to 11 GB to the GPU, so even the lighter mode (estimated 8 GB)
///   leaves almost no room for the system.
/// - at or below `AUTO_BALANCED_MAX_GB`: `balanced`. A 24 GB Mac swaps at 10.3 GB (reported);
///   the estimated 8 GB peak of `balanced` leaves headroom.
/// - above: `performance`. 32 GB and larger hold 10.3 GB comfortably.
pub const AUTO_LOW_MAX_GB: f64 = 16.5;
pub const AUTO_BALANCED_MAX_GB: f64 = 24.5;

/// Estimated resident peak of the model server, GB, at 512x512 / 20 steps. `performance` is the
/// measured 10.3; the other two are ESTIMATES from the weight sizes (balanced: transformer 6.0 +
/// VAE 0.7 + working buffers about 1.5 + one streamed encoder segment; low: a 4 GiB segment
/// budget + working buffers). Replace with measurements (docs/E2E.md, "Real-model runs and
/// memory").
pub const ESTIMATED_PEAK_GB: [(Mode, f64); 3] = [
    (Mode::Performance, 10.3),
    (Mode::Balanced, 8.0),
    (Mode::Low, 5.5),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Performance,
    Balanced,
    Low,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Performance, Mode::Balanced, Mode::Low];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Performance => "performance",
            Mode::Balanced => "balanced",
            Mode::Low => "low",
        }
    }

    /// The exact `sd-server` arguments of the mode, added after the profile's own. The one place
    /// that maps a mode to flags.
    pub fn flags(self) -> Vec<String> {
        let list: &[&str] = match self {
            Mode::Performance => &[],
            Mode::Balanced => &["--params-backend", "te=disk", "--vae-tiling"],
            Mode::Low => &[
                "--params-backend",
                "disk",
                "--max-vram",
                "4",
                "--disable-prefetch",
                "--vae-tiling",
            ],
        };
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Only the option names (`--x`), for the capability check.
    pub fn flag_names(self) -> Vec<String> {
        self.flags()
            .into_iter()
            .filter(|f| f.starts_with("--"))
            .collect()
    }

    pub fn estimated_peak_gb(self) -> f64 {
        ESTIMATED_PEAK_GB
            .iter()
            .find(|(m, _)| *m == self)
            .map(|(_, gb)| *gb)
            .unwrap_or(10.3)
    }
}

/// The `auto` rule. Pure. Unknown memory counts as `balanced` (neither assumes plenty nor the
/// worst).
pub fn auto_mode(total_gb: Option<f64>) -> Mode {
    match total_gb {
        Some(gb) if gb <= AUTO_LOW_MAX_GB => Mode::Low,
        Some(gb) if gb <= AUTO_BALANCED_MAX_GB => Mode::Balanced,
        Some(_) => Mode::Performance,
        None => Mode::Balanced,
    }
}

/// What a run uses, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    /// What was asked for: `auto`, `performance`, `balanced` or `low`.
    pub requested: String,
    /// `env`, `setting` or `default`.
    pub source: String,
    pub mode: Mode,
    pub total_gb: Option<f64>,
    pub flags: Vec<String>,
    /// ESTIMATE (see [`ESTIMATED_PEAK_GB`]).
    pub estimated_peak_gb: f64,
}

/// Parse a value of the setting or the environment variable.
pub fn parse_requested(value: &str) -> Result<Option<Mode>, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" | "" => Ok(None),
        "performance" => Ok(Some(Mode::Performance)),
        "balanced" => Ok(Some(Mode::Balanced)),
        "low" => Ok(Some(Mode::Low)),
        other => Err(format!(
            "invalid_request: memory mode must be auto, performance, balanced or low (got {other:?})"
        )),
    }
}

/// Pure selection from the three inputs. `env` wins over `setting`, then the default.
/// The default is `performance` (today's exact arguments): `auto` is opt-in until the three
/// modes have been measured on both machines (RightKit ruling 2026-10-12).
pub fn select(
    env: Option<&str>,
    setting: Option<&str>,
    total_gb: Option<f64>,
) -> Result<Selection, String> {
    let (requested, source) = match (
        env.filter(|v| !v.trim().is_empty()),
        setting.filter(|v| !v.trim().is_empty()),
    ) {
        (Some(v), _) => (v.trim().to_ascii_lowercase(), "env"),
        (None, Some(v)) => (v.trim().to_ascii_lowercase(), "setting"),
        (None, None) => ("performance".to_string(), "default"),
    };
    let mode = parse_requested(&requested)?.unwrap_or_else(|| auto_mode(total_gb));
    Ok(Selection {
        requested,
        source: source.to_string(),
        mode,
        total_gb,
        flags: mode.flags(),
        estimated_peak_gb: mode.estimated_peak_gb(),
    })
}

/// Total physical memory, GiB: the model manager's probe (`<PREFIX>_HW_JSON` wins, then
/// `RIGHTKIT_HW_JSON`, then the machine).
pub fn total_memory_gb() -> Option<f64> {
    use rightkit_models::{Hardware, HardwareOverride};
    let raw = crate::host::env_var("HW_JSON");
    Hardware::probe_with(raw.as_deref().map(HardwareOverride::Json))
        .unwrap_or_else(|_| Hardware::probe())
        .memory_gb
}

/// Selection for this process: the environment override, the saved `setting`, this machine.
pub fn current(setting: Option<&str>) -> Result<Selection, String> {
    let env = crate::host::env_var(ENV);
    select(env.as_deref(), setting, total_memory_gb())
}

/// Estimated peak of the mode this machine uses by default, for readiness numbers. Falls back to
/// the performance figure when the setting is unreadable.
pub fn current_estimated_peak_gb(setting: Option<&str>) -> f64 {
    current(setting)
        .map(|s| s.estimated_peak_gb)
        .unwrap_or(10.3)
}

// ---------------------------------------------------------------- capability check

/// Option names (`--x`) listed by `<binary> --help`. Run only on a binary whose integrity check
/// has passed. The patched server wants a session token even for `--help`, so a throwaway value
/// is passed (it is never a credential). Cached per path for the life of the process.
pub fn supported_flags(binary: &Path) -> Result<HashSet<String>, String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, HashSet<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = cache.lock().unwrap().get(binary) {
        return Ok(found.clone());
    }
    let mut command = rightkit_child_process::OwnedCommand::new(binary);
    command
        .command_mut()
        .arg("--help")
        .env("SDCPP_SESSION_TOKEN", "help-only")
        .env("SDCPP_SESSION_ID", "help-only");
    const HELP_CAP: usize = 256 * 1024 * 1024;
    let options = rightkit_child_process::RunOptions::new(Duration::from_secs(10))
        .stdout_cap(HELP_CAP)
        .stderr_cap(HELP_CAP);
    let output = rightkit_child_process::run_command(command, None, &options)
        .map_err(|e| format!("run {} --help: {e}", binary.display()))?;
    if output.timed_out {
        return Err(format!(
            "{} --help did not finish in 10 s",
            binary.display()
        ));
    }
    let (stdout, stderr) = (output.stdout.bytes, output.stderr.bytes);
    let mut text = String::from_utf8_lossy(&stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&stderr));
    let flags = parse_help_flags(&text);
    if flags.is_empty() {
        return Err(format!("{} --help listed no options", binary.display()));
    }
    cache
        .lock()
        .unwrap()
        .insert(binary.to_path_buf(), flags.clone());
    Ok(flags)
}

/// Option names at the start of help lines (`  -t, --threads <int>   ...`); words inside
/// descriptions are ignored.
pub fn parse_help_flags(help: &str) -> HashSet<String> {
    let mut found = HashSet::new();
    for line in help.lines() {
        let line = line.trim_start();
        if !line.starts_with('-') {
            continue;
        }
        for token in line.split_whitespace().take(3) {
            let token = token.trim_end_matches(',');
            if token.starts_with("--") && token.len() > 2 {
                found.insert(token.to_string());
            } else if !token.starts_with('-') {
                break;
            }
        }
    }
    found
}

/// The first option of `mode` the server does not list, if any.
pub fn first_unsupported(mode: Mode, supported: &HashSet<String>) -> Option<String> {
    mode.flag_names()
        .into_iter()
        .find(|flag| !supported.contains(flag))
}

/// Typed error for a mode the server cannot honour. Never falls back to another mode.
pub fn unsupported_error(mode: Mode, flag: &str) -> String {
    format!(
        "memory_mode_unsupported: this model server does not accept {flag}, which memory mode '{}' needs; \
         choose another memory mode (performance needs no extra option)",
        mode.as_str()
    )
}

/// After a failed start: when the server's recent output names one of the mode's options, say so.
pub fn rejected_flag(mode: Mode, backend_log: &str) -> Option<String> {
    // The server may echo its arguments, so naming an option is not enough: the log must also say
    // an argument was refused.
    let log = backend_log.to_ascii_lowercase();
    let refused = [
        "unknown",
        "invalid",
        "unrecognized",
        "unsupported",
        "usage:",
    ]
    .iter()
    .any(|w| log.contains(w));
    if !refused {
        return None;
    }
    mode.flag_names()
        .into_iter()
        .find(|flag| backend_log.contains(flag.as_str()))
}
