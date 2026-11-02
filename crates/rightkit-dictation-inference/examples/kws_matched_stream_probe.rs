//! Nonshipping bounded replay: production scheduled cascade vs experimental
//! continuous full phrases. Candidate receives hashes/PCM/settings, no labels.
#[cfg(target_os = "windows")]
mod probe {
    use rightkit_dictation_inference::kws::Kws;
    use rightkit_text_polish::text_pipeline::parse_control_command;
    use serde::Deserialize;
    use serde_json::json;
    use std::{fs, io::Write, time::Instant};
    #[derive(Deserialize)]
    struct Input {
        id: String,
        path: String,
        sha256: String,
    }
    #[derive(Deserialize)]
    struct Arm {
        label: String,
        model_dir: String,
        continuous: bool,
        #[serde(default)]
        fresh_only: bool,
        #[serde(default = "fresh_window")]
        fresh_window_samples: usize,
        #[serde(default = "default_paths")]
        paths: i32,
    }
    fn fresh_window() -> usize {
        24000
    }
    fn default_paths() -> i32 {
        8
    }
    #[derive(Deserialize)]
    struct Plan {
        manifest_sha256: String,
        arms: Vec<Arm>,
        repeats: usize,
    }
    fn hash(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_bytes(bytes)
    }
    fn cpu_ms() -> f64 {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let mut c: FILETIME = unsafe { std::mem::zeroed() };
        let mut e = c;
        let mut k = c;
        let mut u = c;
        assert_ne!(
            unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) },
            0
        );
        let ticks =
            |t: FILETIME| ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64;
        (ticks(k) + ticks(u)) / 10000.0
    }
    fn replay(kws: &mut Kws, audio: &[f32], arm: &Arm) -> Result<serde_json::Value, String> {
        kws.reset_stream()?;
        let mut pending = false;
        let mut last_full = 0usize;
        let mut hit = None;
        let mut elapsed_ms = 0.0;
        let cpu_start = cpu_ms();
        let mut processed = 0;
        let mut next_fresh = 4160;
        // Same 20ms arrival snapshots, 50ms native checkpoints & trailing
        // three-second PCM. EOF gets one globally fixed 200ms flush per arm.
        for total in (4160..audio.len() + 320)
            .step_by(320)
            .map(|n| n.min(audio.len()))
        {
            let origin = total.saturating_sub(48000);
            if arm.fresh_only && total < next_fresh && total != audio.len() {
                continue;
            }
            next_fresh = total + 4000;
            let started = Instant::now();
            let (result, onset) = if arm.fresh_only {
                let fresh_origin = total.saturating_sub(arm.fresh_window_samples);
                kws.transcribe_fresh_full_command_for_replay(
                    &audio[fresh_origin..total],
                    fresh_origin,
                )?
            } else if arm.continuous {
                kws.transcribe_continuous_full_command_result(&audio[origin..total], origin, total)?
            } else {
                let full = pending && total.saturating_sub(last_full) >= 800;
                if full {
                    last_full = total;
                }
                let (r, o, _) = kws.transcribe_scheduled_probe_result_with_wake_start(
                    &audio[origin..total],
                    origin,
                    total,
                    pending,
                    full,
                )?;
                (r, o)
            };
            elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
            processed = total;
            if parse_control_command(&result.text).is_some() {
                hit = Some(json!({"text":result.text,"action_sample":total,"onset_sample":onset}));
                break;
            }
            pending = result.text.trim() == "zephyr";
        }
        Ok(
            json!({"hit":hit,"elapsed_ms":elapsed_ms,"cpu_ms":cpu_ms()-cpu_start,"processed_samples":processed}),
        )
    }
    pub fn run() -> Result<(), String> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.len() != 3 {
            return Err("usage: kws_matched_stream_probe manifest plan output.jsonl".into());
        }
        let bytes = fs::read(&args[0]).map_err(|e| e.to_string())?;
        let plan: Plan = serde_json::from_slice(&fs::read(&args[1]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if hash(&bytes) != plan.manifest_sha256
            || !(1..=3).contains(&plan.repeats)
            || !(1..=3).contains(&plan.arms.len())
        {
            return Err("invalid frozen plan".into());
        }
        let inputs: Vec<Input> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if inputs.len() > 20 || inputs.is_empty() {
            return Err("bounded input count exceeded".into());
        }
        let mut audio = Vec::new();
        for input in &inputs {
            if hash(&fs::read(&input.path).map_err(|e| e.to_string())?) != input.sha256 {
                return Err("PCM hash mismatch".into());
            }
            let wav = rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(&input.path))
                .map_err(|e| e.to_string())?;
            if wav.sample_rate != 16000 {
                return Err("expected mono PCM16 16k".into());
            }
            let mut pcm = wav.samples;
            pcm.extend_from_slice(&[0.0; 3200]);
            audio.push(pcm);
        }
        // The spotter runs on the runtime rightkit-onnx binds.
        rightkit_onnx::configure_runtime_with_options(
            [rightkit_onnx::RuntimeCandidate::new(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../src-tauri/resources/runtime/onnxruntime.dll"),
                rightkit_onnx::CandidateSource::Explicit,
            )],
            &rightkit_onnx::RuntimeBindingOptions {
                force_replace: true,
                ..Default::default()
            },
        )
        .map_err(|e| format!("bind bundled ONNX Runtime: {e}"))?;
        let mut models = plan
            .arms
            .iter()
            .map(|a| Kws::load_for_replay_with_paths(std::path::Path::new(&a.model_dir), a.paths))
            .collect::<Result<Vec<_>, _>>()?;
        for (arm, kws) in plan.arms.iter().zip(models.iter_mut()) {
            replay(kws, &audio[0], arm)?;
        }
        let mut out = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&args[2])
            .map_err(|e| e.to_string())?;
        let start = Instant::now();
        for repeat in 0..plan.repeats {
            for (index, input) in inputs.iter().enumerate() {
                for rotate in 0..plan.arms.len() {
                    if start.elapsed().as_secs() > 180 {
                        return Err("bounded replay deadline exceeded".into());
                    }
                    let i = (rotate + index + repeat) % plan.arms.len();
                    let result = replay(&mut models[i], &audio[index], &plan.arms[i]);
                    let row = match result {
                        Ok(mut row) => {
                            row["id"] = json!(input.id);
                            row["arm"] = json!(plan.arms[i].label);
                            row["repeat"] = json!(repeat);
                            row
                        }
                        Err(e) => {
                            json!({"id":input.id,"arm":plan.arms[i].label,"repeat":repeat,"error":e})
                        }
                    };
                    serde_json::to_writer(&mut out, &row).map_err(|e| e.to_string())?;
                    writeln!(out).map_err(|e| e.to_string())?;
                    out.flush().map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
}
#[cfg(target_os = "windows")]
fn main() {
    if let Err(e) = probe::run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "windows"))]
fn main() {
    panic!("Windows bounded probe only");
}
