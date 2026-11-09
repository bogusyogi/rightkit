//! Replay the streaming cascade on rolling 3 s snapshots at absolute sample positions.
//! Stops at the first confirmed command, or submits the final partial snapshot
//! on the live cadence. `--jsonl` emits one row per WAV.
//!
//! D4 fricative estimator (default off): `--d4` enables it; `--d4-window MS`, `--d4-floor MS`,
//! `--d4-rise F`, `--d4-min-run MS`, `--d4-vowel MS`, `--d4-tilt F` set each value, and
//! `--d4-json JSON|FILE` sets any of `enabled`, `window_ms`, `floor_ms`, `rise`, `min_run_ms`,
//! `vowel_ms`, `tilt_margin` (flags apply in order). With D4 on, each clip reports the D4
//! estimate (sample and feature frame) or its abstain reason, and the run prints the full
//! settings first and counts last. The wake and confirm D4 estimates are reported and counted
//! separately (the confirm stage's estimate says nothing about the wake candidate's cut that
//! the command returns), and candidate coverage is counted apart from selected-cut coverage
//! (`selected`, legacy-control `fallback` to emission, `no_boundary`). A feature frame is
//! null (`feature_mapping`: `no_origin` | `before_origin`) when the sample cannot be mapped.
//! `--d4-strict` (needs `--onset fricative`) returns no boundary when D4 has no candidate.
//!
//! Firing and no-fire trace (default off; observation only, detections are identical on and
//! off): `--trace` snapshots every firing at its pre-reset beam event; `--trace-window FRAMES`
//! records every decoded encoder frame on the confirm stream within FRAMES (40 ms each) of the
//! wake candidate, so a clip that never fires still shows the beam; `--trace-capacity STEPS`
//! bounds that window (default 512; a dropped step is counted); `--trace-history TOKENS` caps
//! each recorded path history to its last TOKENS entries (default 16; lengths and a
//! `truncated` flag are recorded). Each clip's JSONL row gains a
//! `trace` key (absent with the trace off) and a text run prints a `TRACE` line.

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), String> {
    native::run(std::env::args_os().skip(1))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use rightkit_dictation_engine::kws::{
        AcousticSearch, FiringTrace, FricativeParams, Kws, KwsFeedEvent, KwsOnsetMethod,
        KwsSwitches, KwsTrace, KwsTraceConfig, StepTrace, TraceClock, TraceExtension,
        TraceHypothesis, TraceMerge, TracePath,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const SNAPSHOT_SAMPLES: usize = 48_000;
    /// Samples per feature frame (10 ms at 16 kHz).
    const FEATURE_SHIFT_SAMPLES: usize = 160;
    const USAGE: &str = "usage: kws_onset_replay <model-dir> <wav>... [--ort LIB] \
        [--step SAMPLES] [--onset emission|aligned|posterior|acoustic|fricative] \
        [--acoustic-search legacy|initial-fricative] [--d4] [--d4-window MS] [--d4-floor MS] \
        [--d4-rise F] [--d4-min-run MS] [--d4-vowel MS] [--d4-tilt F] [--d4-json JSON|FILE] \
        [--d4-strict] [--trace] [--trace-window FRAMES] [--trace-capacity STEPS] [--trace-history TOKENS] \
        [--jsonl]";

    fn flag_value<I: Iterator<Item = OsString>>(
        args: &mut I,
        flag: &str,
    ) -> Result<String, String> {
        args.next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| format!("{flag} needs a value"))
    }

    fn parse_u32(text: &str, flag: &str) -> Result<u32, String> {
        text.parse::<u32>()
            .map_err(|_| format!("{flag} needs a whole number, got {text:?}"))
    }

    fn parse_f32(text: &str, flag: &str) -> Result<f32, String> {
        text.parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{flag} needs a finite number, got {text:?}"))
    }

    fn json_u32(value: &serde_json::Value, key: &str) -> Result<u32, String> {
        value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .ok_or_else(|| format!("--d4-json {key} needs a whole number"))
    }

    fn json_f32(value: &serde_json::Value, key: &str) -> Result<f32, String> {
        value
            .as_f64()
            .map(|number| number as f32)
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("--d4-json {key} needs a finite number"))
    }

    /// Apply a JSON object (inline text, or a file holding it) to the D4 settings.
    fn apply_d4_json(params: &mut FricativeParams, source: &str) -> Result<(), String> {
        let text = if Path::new(source).is_file() {
            std::fs::read_to_string(source)
                .map_err(|error| format!("--d4-json {source}: {error}"))?
        } else {
            source.to_owned()
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|error| format!("--d4-json: {error}"))?;
        let object = value.as_object().ok_or("--d4-json needs a JSON object")?;
        for (key, value) in object {
            match key.as_str() {
                "enabled" => {
                    params.enabled = value
                        .as_bool()
                        .ok_or("--d4-json enabled needs true or false")?;
                }
                "window_ms" => params.window_ms = json_u32(value, key)?,
                "floor_ms" => params.floor_ms = json_u32(value, key)?,
                "rise" => params.rise = json_f32(value, key)?,
                "min_run_ms" => params.min_run_ms = json_u32(value, key)?,
                "vowel_ms" => params.vowel_ms = json_u32(value, key)?,
                "tilt_margin" => params.tilt_margin = json_f32(value, key)?,
                _ => return Err(format!("--d4-json: unknown key {key:?}")),
            }
        }
        Ok(())
    }

    fn d4_settings_json(params: &FricativeParams) -> serde_json::Value {
        json!({
            "enabled": params.enabled,
            "window_ms": params.window_ms,
            "floor_ms": params.floor_ms,
            "rise": params.rise,
            "min_run_ms": params.min_run_ms,
            "vowel_ms": params.vowel_ms,
            "tilt_margin": params.tilt_margin,
        })
    }

    fn clock_json(clock: &TraceClock) -> serde_json::Value {
        json!({
            "epoch": clock.epoch,
            "epoch_origin_feature_frame": clock.epoch_origin_feature_frame,
            "feature_frames_ready": clock.feature_frames_ready,
            "encoder_window_end_feature_frame": clock.encoder_window_end_feature_frame,
            "encoder_consumed_feature_frames": clock.encoder_consumed_feature_frames,
            "samples_fed": clock.samples_fed,
            "padding_samples_fed": clock.padding_samples_fed,
        })
    }

    fn path_json(path: &TracePath) -> serde_json::Value {
        json!({
            "first_token_frame": path.first_token_frame,
            "level": path.level,
            "context_state": path.context_state,
            "log_prob": path.log_prob,
        })
    }

    fn hypothesis_json(hypothesis: &TraceHypothesis) -> serde_json::Value {
        json!({
            "selected": hypothesis.selected,
            "ys": hypothesis.ys,
            "timestamps": hypothesis.timestamps,
            "ys_probs": hypothesis.ys_probs,
            "ys_len": hypothesis.ys_len,
            "timestamps_len": hypothesis.timestamps_len,
            "ys_probs_len": hypothesis.ys_probs_len,
            "truncated": hypothesis.truncated,
            "log_prob": hypothesis.log_prob,
            "context_state": hypothesis.context_state,
            "level": hypothesis.level,
            "num_trailing_blanks": hypothesis.num_trailing_blanks,
        })
    }

    fn merge_json(merge: &TraceMerge) -> serde_json::Value {
        json!({
            "context_state": merge.context_state,
            "incoming_context_state": merge.incoming_context_state,
            "level": merge.level,
            "existing": {
                "timestamps": merge.existing.timestamps,
                "ys_probs": merge.existing.ys_probs,
                "timestamps_len": merge.existing.timestamps_len,
                "ys_probs_len": merge.existing.ys_probs_len,
                "truncated": merge.existing.truncated,
                "log_prob": merge.existing.log_prob,
            },
            "incoming": {
                "timestamps": merge.incoming.timestamps,
                "ys_probs": merge.incoming.ys_probs,
                "timestamps_len": merge.incoming.timestamps_len,
                "ys_probs_len": merge.incoming.ys_probs_len,
                "truncated": merge.incoming.truncated,
                "log_prob": merge.incoming.log_prob,
            },
            "incoming_kept": merge.incoming_kept,
            "merged_log_prob": merge.merged_log_prob,
        })
    }

    fn firing_json(firing: &FiringTrace) -> serde_json::Value {
        json!({
            "occurrence": firing.occurrence,
            "epoch_occurrence": firing.epoch_occurrence,
            "keyword": firing.keyword,
            "clock": clock_json(&firing.clock),
            "context": firing.context,
            "ys": firing.ys,
            "timestamps": firing.timestamps,
            "ys_probs": firing.ys_probs,
            "ys_len": firing.ys_len,
            "timestamps_len": firing.timestamps_len,
            "ys_probs_len": firing.ys_probs_len,
            "truncated": firing.truncated,
            "fired_frame": firing.fired_frame,
            "num_trailing_blanks": firing.num_trailing_blanks,
            "score": firing.score,
            "threshold": firing.threshold,
            "hypotheses_total": firing.hypotheses_total,
            "hypotheses": firing.hypotheses.iter().map(hypothesis_json).collect::<Vec<_>>(),
        })
    }

    fn extension_json(extension: &TraceExtension) -> serde_json::Value {
        json!({
            "parent_context_state": extension.parent_context_state,
            "parent_level": extension.parent_level,
            "parent_first_token_frame": extension.parent_first_token_frame,
            "token": extension.token,
            "token_prob": extension.token_prob,
            "score": extension.score,
            "rank": extension.rank,
            "selected": extension.selected,
            "cutoff_score": extension.cutoff_score,
            "sibling_token": extension.sibling_token,
            "sibling_score": extension.sibling_score,
        })
    }

    fn step_json(step: &StepTrace) -> serde_json::Value {
        json!({
            "frame": step.frame,
            "clock": clock_json(&step.clock),
            "hypotheses_in": step.hypotheses_in,
            "hypotheses_out": step.hypotheses_out,
            "refilled_blank": step.refilled_blank,
            "top": step.top.as_ref().map(|top| json!({
                "context_state": top.context_state,
                "level": top.level,
                "first_token_frame": top.first_token_frame,
                "log_prob": top.log_prob,
                "num_trailing_blanks": top.num_trailing_blanks,
                "ys_probs": top.ys_probs,
                "ys_probs_len": top.ys_probs_len,
                "truncated": top.truncated,
            })),
            "matched": step.matched.as_ref().map(|matched| json!({
                "score": matched.score,
                "threshold": matched.threshold,
                "trailing_blanks_ok": matched.trailing_blanks_ok,
            })),
            "fired": step.fired,
            "expired": step.expired.iter().map(path_json).collect::<Vec<_>>(),
            "pruned": step.pruned.iter().map(path_json).collect::<Vec<_>>(),
            "merges": step.merges.iter().map(merge_json).collect::<Vec<_>>(),
            "extensions": step.extensions.iter().map(extension_json).collect::<Vec<_>>(),
            "extensions_omitted": step.extensions_omitted,
        })
    }

    fn feed_json(event: &KwsFeedEvent) -> serde_json::Value {
        json!({
            "stage": event.stage,
            "event": event.event,
            "epoch": event.epoch,
            "snapshot_start_sample": event.snapshot_start_sample,
            "live_total_sample": event.live_total_sample,
            "origin_sample": event.origin_sample,
            "fed_until_sample": event.fed_until_sample,
            "accepted_until_sample": event.accepted_until_sample,
            "pending_samples": event.pending_samples,
            "candidate_id": event.candidate_id,
            "candidate_start_sample": event.candidate_start_sample,
            "candidate_end_sample": event.candidate_end_sample,
            "expiry_deadline_sample": event.expiry_deadline_sample,
        })
    }

    /// Everything one clip's replay drained. `truncated` is true when any buffer dropped a
    /// record for capacity: the record is then incomplete.
    fn trace_json(traces: &[KwsTrace]) -> serde_json::Value {
        let mut streams = Vec::new();
        let mut feeds = Vec::new();
        let mut streams_dropped = 0_u64;
        let mut feeds_dropped = 0_u64;
        let mut truncated = false;
        for trace in traces {
            streams_dropped += trace.streams_dropped;
            feeds_dropped += trace.feeds_dropped;
            for stream in &trace.streams {
                truncated |= stream.records.truncated();
                let identity = &stream.records.identity;
                streams.push(json!({
                    "stage": stream.stage,
                    "origin_sample": stream.origin_sample,
                    "candidate_id": stream.candidate_id,
                    "window": stream.window,
                    "real_samples_fed": stream.real_samples_fed,
                    "tail_samples_fed": stream.tail_samples_fed,
                    "epoch": stream.epoch,
                    "identity": {
                        "crate_version": identity.crate_version,
                        "keywords_fnv64": format!("{:016x}", identity.keywords_fnv64),
                        "config_fnv64": format!("{:016x}", identity.config_fnv64),
                        "model_bytes": identity.model_bytes,
                    },
                    "firings": stream.records.firings.iter().map(firing_json).collect::<Vec<_>>(),
                    "firings_dropped": stream.records.firings_dropped,
                    "steps": stream.records.steps.iter().map(step_json).collect::<Vec<_>>(),
                    "steps_dropped": stream.records.steps_dropped,
                }));
            }
            feeds.extend(trace.feeds.iter().map(feed_json));
        }
        json!({
            "truncated": truncated || streams_dropped > 0 || feeds_dropped > 0,
            "streams_dropped": streams_dropped,
            "feeds_dropped": feeds_dropped,
            "streams": streams,
            "feeds": feeds,
        })
    }

    struct Options {
        model: PathBuf,
        wavs: Vec<PathBuf>,
        ort: Option<PathBuf>,
        step: usize,
        switches: KwsSwitches,
        jsonl: bool,
    }

    fn parse_options(mut args: impl Iterator<Item = OsString>) -> Result<Options, String> {
        let mut paths = Vec::new();
        let mut ort = None;
        let mut step = 320;
        let mut switches = KwsSwitches::default();
        let mut trace = KwsTraceConfig {
            capacity_steps: KwsTraceConfig::DEFAULT_CAPACITY_STEPS,
            ..KwsTraceConfig::default()
        };
        let mut trace_on = false;
        let mut jsonl = false;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--") => {
                    paths.extend(args.map(PathBuf::from));
                    break;
                }
                Some("--ort") => {
                    ort = Some(PathBuf::from(
                        args.next()
                            .filter(|path| !path.to_string_lossy().starts_with('-'))
                            .ok_or("--ort needs a path")?,
                    ));
                }
                Some("--step") => {
                    step = args
                        .next()
                        .and_then(|value| value.to_str()?.parse::<usize>().ok())
                        .filter(|step| (1..=SNAPSHOT_SAMPLES).contains(step))
                        .ok_or("--step must be 1 to 48000 samples")?;
                }
                Some("--onset") => {
                    switches.onset = Some(match args.next().as_deref().and_then(|a| a.to_str()) {
                        Some("emission") => KwsOnsetMethod::Emission,
                        Some("aligned") => KwsOnsetMethod::Aligned,
                        Some("posterior") => KwsOnsetMethod::Posterior,
                        Some("acoustic") => KwsOnsetMethod::Acoustic,
                        Some("fricative") => KwsOnsetMethod::Fricative,
                        _ => {
                            return Err(
                                "--onset needs emission, aligned, posterior, acoustic or fricative"
                                    .into(),
                            );
                        }
                    });
                }
                Some("--acoustic-search") => {
                    switches.acoustic_search = match args.next().as_deref().and_then(|a| a.to_str())
                    {
                        Some("legacy") => AcousticSearch::Legacy,
                        Some("initial-fricative") => AcousticSearch::InitialFricative,
                        _ => {
                            return Err(
                                "--acoustic-search needs legacy or initial-fricative".into()
                            );
                        }
                    };
                }
                Some("--d4") => switches.fricative.enabled = true,
                Some("--d4-window") => {
                    switches.fricative.window_ms =
                        parse_u32(&flag_value(&mut args, "--d4-window")?, "--d4-window")?;
                }
                Some("--d4-floor") => {
                    switches.fricative.floor_ms =
                        parse_u32(&flag_value(&mut args, "--d4-floor")?, "--d4-floor")?;
                }
                Some("--d4-rise") => {
                    switches.fricative.rise =
                        parse_f32(&flag_value(&mut args, "--d4-rise")?, "--d4-rise")?;
                }
                Some("--d4-min-run") => {
                    switches.fricative.min_run_ms =
                        parse_u32(&flag_value(&mut args, "--d4-min-run")?, "--d4-min-run")?;
                }
                Some("--d4-vowel") => {
                    switches.fricative.vowel_ms =
                        parse_u32(&flag_value(&mut args, "--d4-vowel")?, "--d4-vowel")?;
                }
                Some("--d4-tilt") => {
                    switches.fricative.tilt_margin =
                        parse_f32(&flag_value(&mut args, "--d4-tilt")?, "--d4-tilt")?;
                }
                Some("--d4-json") => {
                    let source = flag_value(&mut args, "--d4-json")?;
                    apply_d4_json(&mut switches.fricative, &source)?;
                }
                Some("--d4-strict") => switches.fricative_strict = true,
                Some("--trace") => {
                    trace.firing = true;
                    trace_on = true;
                }
                Some("--trace-window") => {
                    trace.window_frames =
                        parse_u32(&flag_value(&mut args, "--trace-window")?, "--trace-window")?;
                    trace_on = true;
                }
                Some("--trace-capacity") => {
                    trace.capacity_steps = parse_u32(
                        &flag_value(&mut args, "--trace-capacity")?,
                        "--trace-capacity",
                    )? as usize;
                    trace_on = true;
                }
                Some("--trace-history") => {
                    trace.history_tokens = parse_u32(
                        &flag_value(&mut args, "--trace-history")?,
                        "--trace-history",
                    )? as usize;
                    trace_on = true;
                }
                Some("--jsonl") => jsonl = true,
                _ if arg.to_string_lossy().starts_with('-') => {
                    return Err(format!("unknown option {:?}\n{USAGE}", arg));
                }
                _ => paths.push(PathBuf::from(arg)),
            }
        }
        if switches.acoustic_search == AcousticSearch::InitialFricative
            && switches.onset != Some(KwsOnsetMethod::Acoustic)
        {
            return Err("--acoustic-search initial-fricative requires --onset acoustic".into());
        }
        if trace_on {
            switches.trace = Some(trace);
        }
        switches.validate()?;
        let mut paths = paths.into_iter();
        let model = paths.next().ok_or(USAGE)?;
        let wavs: Vec<_> = paths.collect();
        if wavs.is_empty() {
            return Err(USAGE.into());
        }
        Ok(Options {
            model,
            wavs,
            ort,
            step,
            switches,
            jsonl,
        })
    }

    fn bind_runtime(ort: Option<PathBuf>) -> Result<(), String> {
        let src_tauri = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri");
        #[cfg(target_os = "windows")]
        let default_ort = Some(src_tauri.join("resources/runtime/onnxruntime.dll"));
        #[cfg(target_os = "macos")]
        let default_ort = std::env::var_os("ORT_DYLIB_PATH")
            .map(PathBuf::from)
            .into_iter()
            .chain([
                src_tauri.join("resources/runtime/libonnxruntime.dylib"),
                src_tauri.join("native-sources/libonnxruntime.dylib"),
            ])
            .find(|path| path.is_file());
        if let Some(runtime) = ort.or(default_ort) {
            rightkit_onnx::configure_runtime_with_options(
                [rightkit_onnx::RuntimeCandidate::new(
                    runtime,
                    rightkit_onnx::CandidateSource::ExplicitFile,
                )],
                &rightkit_onnx::RuntimeBindingOptions {
                    force_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// One stage's D4 outcome, from that stage's own onset report.
    #[derive(Clone, Copy, Debug, Default)]
    struct D4Stage {
        /// Absolute sample where that stage's stream clock starts.
        origin: Option<usize>,
        /// D4 estimate on the outer sample clock; `None` when it abstained.
        sample: Option<usize>,
        abstain: Option<&'static str>,
        /// The estimate as a feature frame on that stream's own clock (any origin).
        relative_feature: Option<i32>,
        /// `no_origin` when an estimate exists but cannot be placed on the outer clock.
        unmappable: Option<&'static str>,
    }

    impl D4Stage {
        /// The estimate as a feature frame (10 ms) since the stream origin, or why it cannot
        /// be mapped: never a fabricated zero-relative number.
        fn feature(self) -> Result<usize, &'static str> {
            if let Some(why) = self.unmappable {
                return Err(why);
            }
            let sample = self.sample.ok_or("no_sample")?;
            let origin = self.origin.ok_or("no_origin")?;
            sample
                .checked_sub(origin)
                .map(|samples| samples / FEATURE_SHIFT_SAMPLES)
                .ok_or("before_origin")
        }

        /// `ok`, the abstain label, or `no_estimate` (enabled, neither sample nor label);
        /// `absent` when this stage produced no report.
        fn reason(stage: Option<Self>, absent: &'static str) -> &'static str {
            match stage {
                None => absent,
                Some(stage) => match (stage.sample, stage.unmappable, stage.abstain) {
                    (Some(_), _, _) => "ok",
                    (None, Some(_), _) => "unmappable_no_origin",
                    (None, None, Some(reason)) => reason,
                    (None, None, None) => "no_estimate",
                },
            }
        }

        fn feature_text(stage: Option<Self>) -> String {
            match stage.map(|stage| (stage.feature(), stage.relative_feature)) {
                Some((Ok(frame), _)) => frame.to_string(),
                Some((Err(why), Some(relative))) => format!("null({why};relative={relative})"),
                Some((Err(why), None)) => format!("null({why})"),
                None => "null(no_report)".to_owned(),
            }
        }

        fn json(stage: Option<Self>, absent: &'static str) -> serde_json::Value {
            let feature = stage.map(Self::feature);
            json!({
                "reported": stage.is_some(),
                "origin_sample": stage.and_then(|stage| stage.origin),
                "sample": stage.and_then(|stage| stage.sample),
                "feature": feature.and_then(|frame| frame.ok()),
                "feature_mapping": feature.map(|frame| frame.map_or_else(|why| why, |_| "ok")),
                "relative_feature": stage.and_then(|stage| stage.relative_feature),
                "unmappable": stage.and_then(|stage| stage.unmappable),
                "abstain": stage.and_then(|stage| stage.abstain),
                "reason": Self::reason(stage, absent),
            })
        }
    }

    #[derive(Default)]
    struct Replay {
        samples: usize,
        wake_observed: bool,
        command: Option<String>,
        fire_sample: Option<usize>,
        /// The boundary the cascade returned: always the wake candidate's cut.
        boundary_sample: Option<usize>,
        /// The method whose candidate that cut used (`emission` is the legacy fallback;
        /// `no_boundary` is a strict-D4 miss).
        chosen_source: Option<&'static str>,
        /// D4 outcome of the wake report that owns the returned boundary, selected by
        /// candidate identity (the latest adopted wake report when no command latched). Scores the returned boundary.
        wake_d4: Option<D4Stage>,
        /// D4 outcome of the latched confirm report. Scores only the confirm estimator,
        /// never the returned boundary.
        confirm_d4: Option<D4Stage>,
        /// A command latched but its owning wake report was not found (ring truncation or
        /// absent). Confirm evidence is kept; no owner-based claim (`wake_d4`,
        /// `chosen_source`, selected-cut status) is made.
        owner_missing: bool,
        /// What the cascade's trace drained during this clip; empty with the trace off.
        trace: Vec<KwsTrace>,
    }

    impl Replay {
        fn wake_reason(&self) -> &'static str {
            if self.owner_missing {
                return "owner_missing";
            }
            D4Stage::reason(self.wake_d4, "no_wake_report")
        }

        fn confirm_reason(&self) -> &'static str {
            let absent = if self.command.is_some() {
                "no_confirm_report"
            } else {
                "no_command"
            };
            D4Stage::reason(self.confirm_d4, absent)
        }

        /// The returned boundary came directly from D4's candidate (not the emission fallback).
        fn d4_direct_cut(&self) -> bool {
            self.boundary_sample.is_some() && self.chosen_source == Some("fricative")
        }

        fn status(&self) -> &'static str {
            match (&self.command, self.boundary_sample, self.wake_observed) {
                (Some(_), Some(_), _) => "command",
                (Some(_), None, _) => "no_boundary",
                (None, _, true) => "wake_only",
                (None, _, false) => "miss",
            }
        }

        /// How the returned boundary relates to `method`: `selected` (that method's
        /// candidate), `fallback` (legacy-control emission cut, not a success of `method`),
        /// `no_boundary` (a command with no cut), `unavailable` (no command), `unknown`.
        fn candidate_status(&self, method: &str) -> &'static str {
            match (self.boundary_sample, self.chosen_source) {
                _ if self.owner_missing => "owner_missing",
                (None, _) if self.command.is_some() => "no_boundary",
                (None, _) => "unavailable",
                (Some(_), Some(source)) if source == method => "selected",
                (Some(_), Some("emission")) => "fallback",
                (Some(_), _) => "unknown",
            }
        }
    }

    fn arrival_ends(samples: usize, step: usize) -> impl Iterator<Item = usize> {
        (0..samples)
            .step_by(step)
            .map(move |start| start.saturating_add(step).min(samples))
    }

    fn replay_wav(path: &Path, options: &Options) -> Result<Replay, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path).map_err(|e| e.to_string())?;
        if wav.sample_rate != 16_000 || wav.samples.is_empty() {
            return Err("expected nonempty 16 kHz WAV".into());
        }
        let mut kws = Kws::load_with_switches(&options.model, options.switches)?;
        let mut replay = Replay {
            samples: wav.samples.len(),
            ..Replay::default()
        };
        let mut reports = Vec::new();
        for end in arrival_ends(replay.samples, options.step) {
            let start = end.saturating_sub(SNAPSHOT_SAMPLES);
            let (result, boundary) = kws.transcribe_cascade_result_with_wake_start(
                &wav.samples[start..end],
                start,
                end,
                false,
            )?;
            // Drain every step: the cascade keeps only the last 16 reports.
            reports.extend(kws.take_onset_reports());
            replay.trace.extend(kws.take_trace());
            match result.text.as_str() {
                "" => {}
                "zephyr" => replay.wake_observed = true,
                _ => {
                    replay.command = Some(result.text);
                    replay.fire_sample = Some(end);
                    replay.boundary_sample = boundary;
                    break;
                }
            }
        }
        // The latch (or the last step) may have left records in the cascade.
        replay.trace.extend(kws.take_trace());
        let stage = |report: &rightkit_dictation_engine::kws::KwsOnsetReport| D4Stage {
            origin: report.origin_sample,
            sample: report.fricative_sample,
            abstain: report.fricative_abstain,
            relative_feature: report.fricative_relative_feature,
            unmappable: report.fricative_unmappable,
        };
        // The returned cut belongs to the wake candidate the command latched on. Select its
        // wake report by candidate identity (the id the clock assigned when it adopted that
        // candidate, carried by the latched confirm report), never by boundary equality.
        // Without a command: the latest adopted wake report.
        if replay.command.is_some() {
            match rightkit_dictation_engine::kws::latched_owner(&reports) {
                Some((wake, confirm)) => {
                    // The confirm evidence stands on its own; owner claims need the owner.
                    replay.confirm_d4 = Some(stage(confirm));
                    match wake {
                        Some(wake) => {
                            replay.chosen_source = confirm.chosen_source;
                            replay.wake_d4 = Some(stage(wake));
                        }
                        None => replay.owner_missing = true,
                    }
                }
                None => replay.owner_missing = true,
            }
        } else {
            replay.wake_d4 = reports
                .iter()
                .rev()
                .find(|report| report.stage == "wake" && report.adopted == Some(true))
                .map(stage);
        }
        Ok(replay)
    }

    pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
        let options = parse_options(args)?;
        let method = options.switches.onset.map_or("off", KwsOnsetMethod::as_str);
        let search = match options.switches.acoustic_search {
            AcousticSearch::Legacy => "legacy",
            AcousticSearch::InitialFricative => "initial-fricative",
        };
        let runtime = bind_runtime(options.ort.clone());
        let d4 = options.switches.fricative;
        // Candidate coverage per stage, apart from selected-cut coverage.
        let mut wake_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut confirm_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut selected_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut direct_cuts = 0_usize;
        if d4.enabled {
            if options.jsonl {
                println!(
                    "{}",
                    json!({
                        "kind": "d4_header",
                        "onset": method,
                        "acoustic_search": search,
                        "step": options.step,
                        "onset_shift_ms": options.switches.onset_shift_ms,
                        "fricative": d4_settings_json(&d4),
                        "fricative_strict": options.switches.fricative_strict,
                    })
                );
            } else {
                println!(
                    "# d4 settings onset={method} acoustic_search={search} step={} strict={} {}",
                    options.step,
                    options.switches.fricative_strict,
                    d4_settings_json(&d4)
                );
            }
        }
        let mut errors = 0;
        for path in &options.wavs {
            let outcome = match &runtime {
                Ok(()) => replay_wav(path, &options),
                Err(error) => Err(error.clone()),
            };
            if d4.enabled {
                let (wake, confirm, selected) = match outcome.as_ref() {
                    Ok(row) => (
                        row.wake_reason(),
                        row.confirm_reason(),
                        row.candidate_status(method),
                    ),
                    Err(_) => ("error", "error", "error"),
                };
                *wake_counts.entry(wake).or_insert(0) += 1;
                *confirm_counts.entry(confirm).or_insert(0) += 1;
                *selected_counts.entry(selected).or_insert(0) += 1;
                if outcome.as_ref().is_ok_and(Replay::d4_direct_cut) {
                    direct_cuts += 1;
                }
            }
            if options.jsonl {
                let replay = outcome.as_ref().ok();
                let mut row = json!({
                    "path": path.to_string_lossy(),
                    "status": replay.map_or("error", Replay::status),
                    "samples": replay.map(|row| row.samples),
                    "command": replay.and_then(|row| row.command.as_deref()),
                    "fire_sample": replay.and_then(|row| row.fire_sample),
                    "boundary_sample": replay.and_then(|row| row.boundary_sample),
                    "chosen_source": replay.and_then(|row| row.chosen_source),
                    "candidate_status": replay.map(|row| row.candidate_status(method)),
                    "onset": method,
                    "acoustic_search": search,
                    "fricative_enabled": d4.enabled,
                    "fricative_strict": options.switches.fricative_strict,
                    // The returned boundary is the wake candidate's cut; each stage's D4
                    // estimate is its own and scores only that stage.
                    "boundary_stage": replay.and_then(|row| {
                        row.command.as_ref().map(|_| "wake_candidate_cut")
                    }),
                    "d4": replay.filter(|_| d4.enabled).map(|row| json!({
                        "wake": D4Stage::json(
                            row.wake_d4,
                            if row.owner_missing { "owner_missing" } else { "no_wake_report" },
                        ),
                        "confirm": D4Stage::json(
                            row.confirm_d4,
                            if row.command.is_some() { "no_confirm_report" } else { "no_command" },
                        ),
                        "owner_missing": row.owner_missing,
                        "direct_cut": row.d4_direct_cut(),
                    })),
                    "step": options.step,
                    "error": outcome.as_ref().err(),
                });
                if options.switches.trace.is_some() {
                    row["trace"] =
                        replay.map_or(serde_json::Value::Null, |clip| trace_json(&clip.trace));
                }
                println!("{row}");
            } else if let Ok(replay) = &outcome {
                if options.switches.trace.is_some() {
                    println!("TRACE\t{}\t{}", path.display(), trace_json(&replay.trace));
                }
                if d4.enabled {
                    println!(
                        "D4\t{}\twake_sample={:?}\twake_feature={}\twake={}\tconfirm_sample={:?}\tconfirm_feature={}\tconfirm={}\tcut={}\tsource={:?}",
                        path.display(),
                        replay.wake_d4.and_then(|stage| stage.sample),
                        D4Stage::feature_text(replay.wake_d4),
                        replay.wake_reason(),
                        replay.confirm_d4.and_then(|stage| stage.sample),
                        D4Stage::feature_text(replay.confirm_d4),
                        replay.confirm_reason(),
                        replay.candidate_status(method),
                        replay.chosen_source
                    );
                }
                if let (Some(onset), Some(at), Some(text)) =
                    (replay.boundary_sample, replay.fire_sample, &replay.command)
                {
                    println!(
                        "KWS\t{}\t{onset}\t{at}\t{}\t{text}\tstep={}",
                        path.display(),
                        replay.samples,
                        options.step
                    );
                }
            }
            if let Err(error) = outcome {
                errors += 1;
                if !options.jsonl {
                    eprintln!("{}: {error}", path.display());
                }
            }
        }
        if d4.enabled {
            if options.jsonl {
                println!(
                    "{}",
                    json!({
                        "kind": "d4_summary",
                        "wake_candidate_counts": wake_counts,
                        "confirm_candidate_counts": confirm_counts,
                        "selected_cut_method": method,
                        "selected_cut_counts": selected_counts,
                        "d4_direct_cut_clips": direct_cuts,
                    })
                );
            } else {
                println!(
                    "# d4 wake_candidate_counts {} confirm_candidate_counts {} selected_cut({method}) {} d4_direct_cut_clips {direct_cuts}",
                    json!(wake_counts),
                    json!(confirm_counts),
                    json!(selected_counts)
                );
            }
        }
        if errors == 0 {
            Ok(())
        } else {
            Err(format!("{errors} replay input(s) failed"))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn replay_options_require_inputs_and_explicit_acoustic_selection() {
            let parse = |args: &[&str]| parse_options(args.iter().copied().map(OsString::from));
            assert!(parse(&["model"]).is_err());
            assert!(parse(&["model", "wav", "--typo"]).is_err());
            assert!(parse(&["model", "wav", "--acoustic-search", "initial-fricative"]).is_err());
            let options = parse(&[
                "model",
                "wav",
                "--acoustic-search",
                "initial-fricative",
                "--onset",
                "acoustic",
                "--jsonl",
            ])
            .unwrap();
            assert_eq!(
                options.switches.acoustic_search,
                AcousticSearch::InitialFricative
            );
            assert!(options.jsonl);
        }

        #[test]
        fn arrival_ends_cover_short_recordings_and_final_partial_chunk() {
            assert_eq!(arrival_ends(120, 320).collect::<Vec<_>>(), [120]);
            assert_eq!(arrival_ends(701, 320).collect::<Vec<_>>(), [320, 640, 701]);
            assert_eq!(arrival_ends(640, 320).collect::<Vec<_>>(), [320, 640]);
        }

        #[test]
        fn emission_fallback_does_not_count_as_an_acoustic_candidate() {
            let row = Replay {
                command: Some("zephyr stop".into()),
                boundary_sample: Some(320),
                chosen_source: Some("emission"),
                ..Replay::default()
            };
            assert_eq!(row.status(), "command");
            assert_eq!(row.candidate_status("acoustic"), "fallback");
            assert_eq!(row.candidate_status("emission"), "selected");
        }

        #[test]
        fn strict_miss_is_no_boundary_not_a_d4_success() {
            let row = Replay {
                command: Some("zephyr stop".into()),
                boundary_sample: None,
                chosen_source: Some("no_boundary"),
                wake_d4: Some(D4Stage {
                    origin: Some(0),
                    abstain: Some("abstain:no_transition"),
                    ..D4Stage::default()
                }),
                confirm_d4: Some(D4Stage {
                    origin: Some(0),
                    sample: Some(1_600),
                    ..D4Stage::default()
                }),
                ..Replay::default()
            };
            assert_eq!(row.status(), "no_boundary");
            assert_eq!(row.candidate_status("fricative"), "no_boundary");
            assert!(!row.d4_direct_cut());
            assert_eq!(row.wake_reason(), "abstain:no_transition");
            assert_eq!(row.confirm_reason(), "ok");
        }

        #[test]
        fn d4_feature_is_null_without_origin_or_before_it() {
            let stage = |origin, sample| D4Stage {
                origin,
                sample,
                ..D4Stage::default()
            };
            assert_eq!(stage(Some(160), Some(480)).feature(), Ok(2));
            assert_eq!(stage(None, Some(480)).feature(), Err("no_origin"));
            assert_eq!(stage(Some(640), Some(480)).feature(), Err("before_origin"));
            assert_eq!(stage(Some(0), None).feature(), Err("no_sample"));
        }

        #[test]
        fn d4_without_an_origin_is_unmappable_never_ok() {
            // The estimate exists on the stream's clock (frame 12) but has no outer sample.
            let stage = D4Stage {
                relative_feature: Some(12),
                unmappable: Some("no_origin"),
                ..D4Stage::default()
            };
            assert_eq!(stage.feature(), Err("no_origin"));
            assert_eq!(D4Stage::reason(Some(stage), "x"), "unmappable_no_origin");
            assert_eq!(
                D4Stage::feature_text(Some(stage)),
                "null(no_origin;relative=12)"
            );
        }
    }
}
