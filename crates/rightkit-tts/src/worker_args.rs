//! Command line of `rightkit-tts-worker`. Parsing lives in the library so it is tested under the
//! default features; the binary (feature `engine`) only applies defaults and runs the worker.
use crate::mode_location::ModeLocation;
use crate::protocol::Mode;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// Idle window when nothing is given: 600 s.
pub const DEFAULT_IDLE_SECS: u64 = 600;

/// Idle window from the environment: `RIGHTKIT_TTS_IDLE_SECS`, then `GENRIGHT_QTS_IDLE_SECS`,
/// then [`DEFAULT_IDLE_SECS`]. A value that is not a whole number counts as unset. `0` is a zero
/// window: the worker exits as soon as no request is waiting.
pub fn env_idle_timeout() -> Duration {
    let secs = std::env::var("RIGHTKIT_TTS_IDLE_SECS")
        .or_else(|_| std::env::var("GENRIGHT_QTS_IDLE_SECS"))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_SECS);
    Duration::from_secs(secs)
}

/// Parsed worker arguments. Absent options fall back to the binary's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerArgs {
    pub model_dir: Option<PathBuf>,
    pub backend: Option<String>,
    /// Model mode the worker loads for; `base` when `--mode` is absent.
    pub mode: Mode,
    /// Shared ONNX vocoder; defaults to `<model dir>/qwen3-tts-vocoder.onnx`.
    pub vocoder: Option<PathBuf>,
    /// Caller-owned data root for runtime discovery.
    pub data_root: Option<PathBuf>,
    /// ONNX Runtime library the client resolved (overrides discovery).
    pub ort_runtime: Option<PathBuf>,
    /// Per-mode locations (`--mode-model-dir <mode> <dir>`, `--mode-vocoder <mode> <path>`),
    /// delivered before the worker reports ready. `--model-dir` and `--vocoder` stay the location
    /// of the `--mode` the worker starts for; an entry here for that mode takes precedence.
    pub mode_locations: Vec<ModeLocation>,
    /// Idle window in seconds (`--idle-secs`). When present it wins over the environment.
    pub idle_secs: Option<u64>,
}

impl WorkerArgs {
    /// The idle window to run with: `--idle-secs` when given, else [`env_idle_timeout`].
    pub fn idle_timeout(&self) -> Duration {
        self.idle_secs
            .map(Duration::from_secs)
            .unwrap_or_else(env_idle_timeout)
    }

    /// The command line that [`parse`] reads back into an equal value.
    pub fn to_command_line(&self) -> Vec<OsString> {
        let mut out: Vec<OsString> = Vec::new();
        let mut push = |flag: &str, value: OsString| {
            out.push(flag.into());
            out.push(value);
        };
        if let Some(dir) = &self.model_dir {
            push("--model-dir", dir.into());
        }
        if let Some(backend) = &self.backend {
            push("--backend", backend.into());
        }
        push("--mode", self.mode.as_str().into());
        if let Some(vocoder) = &self.vocoder {
            push("--vocoder", vocoder.into());
        }
        if let Some(root) = &self.data_root {
            push("--data-root", root.into());
        }
        if let Some(ort) = &self.ort_runtime {
            push("--ort", ort.into());
        }
        for location in &self.mode_locations {
            out.push("--mode-model-dir".into());
            out.push(location.mode.as_str().into());
            out.push((&location.model_dir).into());
            if let Some(vocoder) = &location.vocoder {
                out.push("--mode-vocoder".into());
                out.push(location.mode.as_str().into());
                out.push(vocoder.into());
            }
        }
        if let Some(secs) = self.idle_secs {
            out.push("--idle-secs".into());
            out.push(secs.to_string().into());
        }
        out
    }
}

fn mode_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<Mode, String> {
    let name = value(args, flag)?;
    Mode::parse(&name)
        .ok_or_else(|| format!("unknown mode {name}; expected base, custom_voice or voice_design"))
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Parse the arguments after the program name. Unknown flags, a flag without its value, and an
/// unknown `--mode` are errors; the binary exits 64 on any of them.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<WorkerArgs, String> {
    let mut out = WorkerArgs::default();
    // Per-mode vocoders may arrive before or after their folder; pair them at the end.
    let mut vocoders: Vec<(Mode, PathBuf)> = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model-dir" => out.model_dir = Some(value(&mut args, &arg)?.into()),
            "--backend" => out.backend = Some(value(&mut args, &arg)?),
            "--mode" => {
                let name = value(&mut args, &arg)?;
                out.mode = Mode::parse(&name).ok_or_else(|| {
                    format!("unknown mode {name}; expected base, custom_voice or voice_design")
                })?;
            }
            "--vocoder" => out.vocoder = Some(value(&mut args, &arg)?.into()),
            "--data-root" => out.data_root = Some(value(&mut args, &arg)?.into()),
            "--ort" => out.ort_runtime = Some(value(&mut args, &arg)?.into()),
            "--mode-model-dir" => {
                let mode = mode_value(&mut args, &arg)?;
                let dir = value(&mut args, &arg)?;
                out.mode_locations.push(ModeLocation::new(mode, dir));
            }
            "--mode-vocoder" => {
                let mode = mode_value(&mut args, &arg)?;
                vocoders.push((mode, value(&mut args, &arg)?.into()));
            }
            "--idle-secs" => {
                let secs = value(&mut args, &arg)?;
                out.idle_secs = Some(
                    secs.parse()
                        .map_err(|_| format!("--idle-secs needs a whole number, got {secs}"))?,
                );
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    for (mode, vocoder) in vocoders {
        let entry = out
            .mode_locations
            .iter_mut()
            .rev()
            .find(|e| e.mode == mode)
            .ok_or_else(|| format!("--mode-vocoder {} has no --mode-model-dir", mode.as_str()))?;
        entry.vocoder = Some(vocoder);
    }
    Ok(out)
}
