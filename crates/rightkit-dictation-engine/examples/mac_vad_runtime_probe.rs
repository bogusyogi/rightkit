//! macOS ORT VAD thread/spinning/startup probe.
//!
//! The measured section excludes model loading, readiness polling, warmup, and
//! WAV decoding. It reports process user+kernel CPU, wall time, exact per-frame
//! verdict bits, and a short post-run idle CPU sample. No production defaults
//! are changed unless a probe environment variable or `--arm` is supplied.
//!
//! Example (run through RightKit on macOS):
//!
//!   RIGHTKIT_VAD_ORT_DYLIB=/path/to/libonnxruntime.dylib \
//!     rustright cargo run --release --example mac_vad_runtime_probe -- \
//!     /path/to/silero_vad_16k_op15.onnx /path/to/clip.wav \
//!     --arm threads2 --startup preinit --iterations 3
//!
//! Arms: `default`, `threads1`, `threads2`, `threads4`, `spin-off`, and
//! `windows-private`. `threadsN` sets both intra/inter to N. `spin-off` sets
//! both spinning controls to 0. `windows-private` combines 1/1, spinning off,
//! and an independent session pool. Startup modes are `lazy`, `preinit`, and
//! `kws-first`; the last loads real KWS from `RIGHTKIT_KWS_DIR` before
//! constructing VAD and optionally decodes a short silence window. Explicit env controls are also accepted:
//! `RIGHTKIT_VAD_ORT_THREADS`, `RIGHTKIT_VAD_ORT_INTRA_THREADS`,
//! `RIGHTKIT_VAD_ORT_INTER_THREADS`, `RIGHTKIT_VAD_ORT_INTRA_SPINNING`,
//! `RIGHTKIT_VAD_ORT_INTER_SPINNING`, and `RIGHTKIT_VAD_ORT_INDEPENDENT_POOL`.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mac_vad_runtime_probe requires macOS");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
mod macos {
    use rightkit_dictation_engine::vad::{initialize_ort_for_probe, SpeechVad};
    use serde_json::json;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const FRAME: usize = 512;
    const DEFAULT_ITERATIONS: usize = 3;
    const DEFAULT_IDLE_MS: u64 = 250;
    const DEFAULT_KWS_SILENCE_MS: u64 = 100;

    fn process_cpu() -> (f64, f64) {
        unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut usage), 0);
            (
                usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1e6,
                usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1e6,
            )
        }
    }

    fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if wav.sample_rate != 16_000_u32 {
            return Err(format!(
                "{}: expected {} Hz WAV, got {} Hz",
                path.display(),
                16_000,
                wav.sample_rate
            ));
        }
        Ok(wav.samples)
    }

    fn set_or_remove(name: &str, value: Option<&str>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    fn configure_arm(arm: &str) -> Result<(), String> {
        match arm {
            "default" => {
                for name in [
                    "RIGHTKIT_VAD_ORT_THREADS",
                    "RIGHTKIT_VAD_ORT_INTRA_THREADS",
                    "RIGHTKIT_VAD_ORT_INTER_THREADS",
                    "RIGHTKIT_VAD_ORT_INTRA_SPINNING",
                    "RIGHTKIT_VAD_ORT_INTER_SPINNING",
                    "RIGHTKIT_VAD_ORT_INDEPENDENT_POOL",
                ] {
                    set_or_remove(name, None);
                }
            }
            "threads1" | "threads2" | "threads4" => {
                let threads = &arm[7..];
                set_or_remove("RIGHTKIT_VAD_ORT_THREADS", Some(threads));
                set_or_remove("RIGHTKIT_VAD_ORT_INTRA_THREADS", None);
                set_or_remove("RIGHTKIT_VAD_ORT_INTER_THREADS", None);
            }
            "spin-off" => {
                set_or_remove("RIGHTKIT_VAD_ORT_INTRA_SPINNING", Some("0"));
                set_or_remove("RIGHTKIT_VAD_ORT_INTER_SPINNING", Some("0"));
            }
            "windows-private" => {
                set_or_remove("RIGHTKIT_VAD_ORT_THREADS", Some("1"));
                set_or_remove("RIGHTKIT_VAD_ORT_INTRA_THREADS", None);
                set_or_remove("RIGHTKIT_VAD_ORT_INTER_THREADS", None);
                set_or_remove("RIGHTKIT_VAD_ORT_INTRA_SPINNING", Some("0"));
                set_or_remove("RIGHTKIT_VAD_ORT_INTER_SPINNING", Some("0"));
                set_or_remove("RIGHTKIT_VAD_ORT_INDEPENDENT_POOL", Some("1"));
            }
            _ => {
                return Err(format!(
                    "unknown arm {arm:?}; expected default, threads1/2/4, spin-off, or windows-private"
                ));
            }
        }
        Ok(())
    }

    fn wait_ready(vad: &mut SpeechVad) -> Result<f64, String> {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(15);
        let silence = [0.0f32; FRAME];
        while Instant::now() < deadline {
            let _ = vad.observe_frames(&silence);
            if vad.backend_name() == Some("ort") {
                return Ok(started.elapsed().as_secs_f64() * 1_000.0);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("ORT VAD did not become ready within 15 seconds".into())
    }

    fn run_frames(vad: &mut SpeechVad, audio: &[f32]) -> Result<String, String> {
        let mut bits = String::with_capacity(audio.len() / FRAME);
        for frame in audio.as_chunks::<FRAME>().0 {
            let verdicts = vad
                .observe_frames(frame)
                .ok_or_else(|| "VAD became unavailable during measured replay".to_string())?;
            if verdicts.len() != 1 {
                return Err(format!(
                    "expected one verdict per 512-sample chunk, got {}",
                    verdicts.len()
                ));
            }
            bits.push(if verdicts[0] { '1' } else { '0' });
        }
        Ok(bits)
    }

    fn option_env(name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    pub fn run() -> Result<(), String> {
        let mut args = std::env::args().skip(1);
        let model = args
            .next()
            .ok_or("usage: mac_vad_runtime_probe MODEL.onnx AUDIO.wav [options]")?;
        let audio_path = args
            .next()
            .ok_or("usage: mac_vad_runtime_probe MODEL.onnx AUDIO.wav [options]")?;
        let mut arm = "env".to_string();
        let mut startup = "lazy".to_string();
        let mut iterations = DEFAULT_ITERATIONS;
        let mut idle_ms = DEFAULT_IDLE_MS;
        let mut kws_silence_ms = DEFAULT_KWS_SILENCE_MS;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--arm" => arm = args.next().ok_or("--arm requires a value")?,
                "--startup" => {
                    startup = args
                        .next()
                        .ok_or("--startup requires lazy, preinit, or kws-first")?
                }
                "--iterations" => {
                    iterations = args
                        .next()
                        .ok_or("--iterations requires a positive integer")?
                        .parse()
                        .map_err(|_| "--iterations requires a positive integer")?;
                    if iterations == 0 {
                        return Err("--iterations requires a positive integer".into());
                    }
                }
                "--idle-ms" => {
                    idle_ms = args
                        .next()
                        .ok_or("--idle-ms requires a non-negative integer")?
                        .parse()
                        .map_err(|_| "--idle-ms requires a non-negative integer")?;
                }
                "--kws-silence-ms" => {
                    kws_silence_ms = args
                        .next()
                        .ok_or("--kws-silence-ms requires a non-negative integer")?
                        .parse()
                        .map_err(|_| "--kws-silence-ms requires a non-negative integer")?;
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        if arm != "env" {
            configure_arm(&arm)?;
        }
        if startup != "lazy" && startup != "preinit" && startup != "kws-first" {
            return Err("--startup requires lazy, preinit, or kws-first".into());
        }
        let audio = read_wav(Path::new(&audio_path))?;
        let startup_ms = if startup == "preinit" {
            let started = Instant::now();
            initialize_ort_for_probe()?;
            Some(started.elapsed().as_secs_f64() * 1_000.0)
        } else {
            None
        };
        let mut kws_guard = None;
        let kws_init_ms = if startup == "kws-first" {
            let kws_dir = std::env::var_os("RIGHTKIT_KWS_DIR")
                .ok_or("--startup kws-first requires RIGHTKIT_KWS_DIR")?;
            let started = Instant::now();
            let mut kws = rightkit_dictation_engine::kws::Kws::load(Path::new(&kws_dir))?;
            if kws_silence_ms > 0 {
                let silence_samples = (kws_silence_ms as usize * 16_000 / 1_000).max(1);
                let silence = vec![0.0f32; silence_samples];
                let _ = kws.transcribe(&silence)?;
                kws.reset_stream()?;
            }
            kws_guard = Some(kws);
            Some(started.elapsed().as_secs_f64() * 1_000.0)
        } else {
            None
        };

        let load_started = Instant::now();
        let mut vad = SpeechVad::with_model_path(Some(model.clone().into()));
        let ready_ms = wait_ready(&mut vad)?;
        let load_to_ready_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        vad.reset();

        // Warmup is deliberately outside measured CPU/wall intervals.
        let warmup_started = Instant::now();
        let _ = run_frames(&mut vad, &audio)?;
        let warmup_ms = warmup_started.elapsed().as_secs_f64() * 1_000.0;
        vad.reset();

        let cpu_before = process_cpu();
        let measured_started = Instant::now();
        let mut output_bits = None;
        let mut repeat_exact = true;
        for _ in 0..iterations {
            vad.reset();
            let bits = run_frames(&mut vad, &audio)?;
            if let Some(previous) = &output_bits {
                repeat_exact &= previous == &bits;
            } else {
                output_bits = Some(bits);
            }
        }
        let measured_wall_ms = measured_started.elapsed().as_secs_f64() * 1_000.0;
        let cpu_after = process_cpu();
        let idle_before = process_cpu();
        let idle_started = Instant::now();
        std::thread::sleep(Duration::from_millis(idle_ms));
        let idle_after = process_cpu();
        let idle_wall_ms = idle_started.elapsed().as_secs_f64() * 1_000.0;

        let user_ms = (cpu_after.0 - cpu_before.0) * 1_000.0;
        let kernel_ms = (cpu_after.1 - cpu_before.1) * 1_000.0;
        let idle_user_ms = (idle_after.0 - idle_before.0) * 1_000.0;
        let idle_kernel_ms = (idle_after.1 - idle_before.1) * 1_000.0;
        let bits = output_bits.unwrap_or_default();
        let speech_frames = bits.bytes().filter(|bit| *bit == b'1').count();
        eprintln!(
            "VAD probe arm={} startup={} frames={} iterations={} wall_ms={:.3} user_ms={:.3} kernel_ms={:.3}",
            arm, startup, bits.len(), iterations, measured_wall_ms, user_ms, kernel_ms
        );
        println!(
            "{}",
            json!({
                "schema": "heardright.mac_vad_runtime_probe.v1",
                "backend": "ort",
                "arm": arm,
                "startup": startup,
                "model": model,
                "audio": audio_path,
                "audio_samples": audio.len(),
                "audio_seconds": audio.len() as f64 / 16_000.0,
                "frames": bits.len(),
                "iterations": iterations,
                "startup_init_ms": startup_ms,
                "kws_init_ms": kws_init_ms,
                "kws_silence_ms": (startup == "kws-first").then_some(kws_silence_ms),
                "vad_probability_and_state": "not exposed by SpeechVad; exact verdict bits are reported",
                "load_to_ready_ms": load_to_ready_ms,
                "ready_poll_ms": ready_ms,
                "warmup_ms": warmup_ms,
                "measured_wall_ms": measured_wall_ms,
                "measured_user_ms": user_ms,
                "measured_kernel_ms": kernel_ms,
                "idle_wall_ms": idle_wall_ms,
                "idle_user_ms": idle_user_ms,
                "idle_kernel_ms": idle_kernel_ms,
                "speech_frames": speech_frames,
                "repeat_output_exact": repeat_exact,
                "per_chunk_output_bits": bits,
                "env": {
                    "RIGHTKIT_VAD_ORT_THREADS": option_env("RIGHTKIT_VAD_ORT_THREADS"),
                    "RIGHTKIT_VAD_ORT_INTRA_THREADS": option_env("RIGHTKIT_VAD_ORT_INTRA_THREADS"),
                    "RIGHTKIT_VAD_ORT_INTER_THREADS": option_env("RIGHTKIT_VAD_ORT_INTER_THREADS"),
                    "RIGHTKIT_VAD_ORT_INTRA_SPINNING": option_env("RIGHTKIT_VAD_ORT_INTRA_SPINNING"),
                    "RIGHTKIT_VAD_ORT_INTER_SPINNING": option_env("RIGHTKIT_VAD_ORT_INTER_SPINNING"),
                    "RIGHTKIT_VAD_ORT_INDEPENDENT_POOL": option_env("RIGHTKIT_VAD_ORT_INDEPENDENT_POOL"),
                }
            })
        );
        drop(kws_guard);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() {
    if let Err(error) = macos::run() {
        eprintln!("mac_vad_runtime_probe: {error}");
        std::process::exit(1);
    }
}
