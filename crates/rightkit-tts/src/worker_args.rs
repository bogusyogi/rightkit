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
    Mode::parse(&name).ok_or_else(|| {
        format!("unknown mode {name}; expected base, custom_voice or voice_design")
    })
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
            .ok_or_else(|| {
                format!(
                    "--mode-vocoder {} has no --mode-model-dir",
                    mode.as_str()
                )
            })?;
        entry.vocoder = Some(vocoder);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_means_base_mode_and_defaults() {
        let parsed = parse(args(&[])).unwrap();
        assert_eq!(parsed, WorkerArgs::default());
        assert_eq!(parsed.mode, Mode::Base);
    }

    #[test]
    fn every_mode_name_is_accepted() {
        for (name, mode) in [
            ("base", Mode::Base),
            ("custom_voice", Mode::CustomVoice),
            ("voice_design", Mode::VoiceDesign),
        ] {
            let parsed = parse(args(&["--mode", name])).unwrap();
            assert_eq!(parsed.mode, mode);
        }
    }

    #[test]
    fn unknown_or_missing_mode_is_rejected_not_ignored() {
        let err = parse(args(&["--mode", "bogus"])).unwrap_err();
        assert!(err.contains("unknown mode bogus"), "{err}");
        assert!(parse(args(&["--mode"]))
            .unwrap_err()
            .contains("--mode needs a value"));
        assert!(parse(args(&["--mode", "customvoice"])).is_err());
    }

    #[test]
    fn all_options_parse_together() {
        let parsed = parse(args(&[
            "--model-dir",
            "/m/qts",
            "--backend",
            "cpu",
            "--mode",
            "voice_design",
            "--vocoder",
            "/v/vocoder.onnx",
            "--data-root",
            "/data",
            "--ort",
            "/rt/libonnxruntime.dylib",
        ]))
        .unwrap();
        assert_eq!(parsed.model_dir, Some(PathBuf::from("/m/qts")));
        assert_eq!(parsed.backend.as_deref(), Some("cpu"));
        assert_eq!(parsed.mode, Mode::VoiceDesign);
        assert_eq!(parsed.vocoder, Some(PathBuf::from("/v/vocoder.onnx")));
        assert_eq!(parsed.data_root, Some(PathBuf::from("/data")));
        assert_eq!(
            parsed.ort_runtime,
            Some(PathBuf::from("/rt/libonnxruntime.dylib"))
        );
    }

    #[test]
    fn unknown_argument_and_valueless_flags_are_errors() {
        assert_eq!(
            parse(args(&["--frobnicate"])).unwrap_err(),
            "unknown argument --frobnicate"
        );
        assert!(parse(args(&["--model-dir"]))
            .unwrap_err()
            .contains("--model-dir needs a value"));
        assert!(parse(args(&["--data-root"]))
            .unwrap_err()
            .contains("--data-root needs a value"));
    }

    #[test]
    fn per_mode_locations_parse_in_either_order_and_pair_by_mode() {
        let parsed = parse(args(&[
            "--mode-vocoder",
            "voice_design",
            "/v/vd.onnx",
            "--mode-model-dir",
            "voice_design",
            "/m/vd",
            "--mode-model-dir",
            "custom_voice",
            "/m/cv",
        ]))
        .unwrap();
        assert_eq!(
            parsed.mode_locations,
            vec![
                ModeLocation::new(Mode::VoiceDesign, "/m/vd").with_vocoder("/v/vd.onnx"),
                ModeLocation::new(Mode::CustomVoice, "/m/cv"),
            ]
        );
    }

    #[test]
    fn per_mode_flags_fail_closed() {
        assert!(parse(args(&["--mode-vocoder", "base", "/v.onnx"]))
            .unwrap_err()
            .contains("has no --mode-model-dir"));
        assert!(parse(args(&["--mode-model-dir", "bogus", "/m"]))
            .unwrap_err()
            .contains("unknown mode bogus"));
        assert!(parse(args(&["--mode-model-dir", "base"]))
            .unwrap_err()
            .contains("needs a value"));
    }

    #[test]
    fn idle_secs_parses_and_rejects_non_numbers() {
        assert_eq!(parse(args(&["--idle-secs", "45"])).unwrap().idle_secs, Some(45));
        assert_eq!(parse(args(&["--idle-secs", "0"])).unwrap().idle_secs, Some(0));
        assert!(parse(args(&["--idle-secs", "soon"]))
            .unwrap_err()
            .contains("whole number"));
        assert!(parse(args(&["--idle-secs"])).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn command_line_round_trips_through_parse() {
        let original = WorkerArgs {
            model_dir: Some("/m/qts".into()),
            backend: Some("cpu".into()),
            mode: Mode::VoiceDesign,
            vocoder: Some("/v/v.onnx".into()),
            data_root: Some("/data".into()),
            ort_runtime: Some("/rt/libonnxruntime.dylib".into()),
            mode_locations: vec![
                ModeLocation::new(Mode::Base, "/m/base").with_vocoder("/v/base.onnx"),
                ModeLocation::new(Mode::CustomVoice, "/m/cv"),
            ],
            idle_secs: Some(0),
        };
        let line: Vec<String> = original
            .to_command_line()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(parse(line).unwrap(), original);
    }
}
