//! LIVE incremental A/B, unattended.
//!
//! For each prompt: start the app's real capture session, speak the prompt
//! through the speakers with `say`, and while audio is STILL ARRIVING a worker
//! thread commits each 15 s window at its quiet cut — the actual shipping
//! behaviour under test. At stop, only the tail is decoded. The full buffer is
//! retained and decoded the shipping (batch) way afterwards for comparison, and
//! both are scored against the script that was spoken.
//!
//! The earlier version of this file recorded first and decoded after, which
//! tested the algorithm and not the live path. Here the worker races capture as
//! it would in the app, so a stalled worker, a dropped window or a double
//! committed tail shows up as a WER or disagreement spike.
//!
//! Usage: incremental_live_ab <model_dir> <prompts.txt> [--out <dir>] [--voice <v>]

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod live {
    use rightkit_audio::capture::{OutDType, SessionConfig};
    use rightkit_dictation_engine::asr::AsrRuntime;
    use rightkit_dictation_engine::coreml_asr::CoreMlParakeet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    const SR: u32 = 16_000;
    const SRU: usize = 16_000;
    const WINDOW: usize = 15 * SRU;
    const PADDING: usize = 2_240 * SRU / 1_000;

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() < 2 {
            return Err(
                "usage: incremental_live_ab <model_dir> <prompts.txt> [--out <dir>] [--voice <v>]"
                    .into(),
            );
        }
        let model_dir = args[0].clone();
        let prompts_path = args[1].clone();
        let mut out_dir = format!(
            ".cache/live-ab/{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        let mut voice = "Samantha".to_string();
        let mut i = 2;
        while i < args.len() {
            match args[i].as_str() {
                "--out" => {
                    if let Some(v) = args.get(i + 1) {
                        out_dir = v.clone();
                    }
                    i += 2;
                }
                "--voice" => {
                    if let Some(v) = args.get(i + 1) {
                        voice = v.clone();
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }

        let prompts: Vec<String> = std::fs::read_to_string(&prompts_path)
            .map_err(|e| format!("read prompts: {e}"))?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        if prompts.is_empty() {
            return Err("no prompts".into());
        }
        let out = std::path::PathBuf::from(&out_dir);
        std::fs::create_dir_all(&out).map_err(|e| format!("mkdir: {e}"))?;

        eprintln!("loading model…");
        let model = CoreMlParakeet::load(std::path::Path::new(&model_dir))
            .map_err(|e| format!("load: {e}"))?;
        let runtime = Arc::new(Mutex::new(AsrRuntime::CoreMlParakeet(model)));
        {
            let mut r = runtime.lock().unwrap();
            let _ = r.transcribe(&vec![0.0f32; SRU]);
        }
        eprintln!("model ready; {} prompts, voice={voice}\n", prompts.len());

        let mut rows: Vec<serde_json::Value> = Vec::new();
        for (idx, prompt) in prompts.iter().enumerate() {
            eprintln!("[{}/{}] speaking…", idx + 1, prompts.len());
            let take = match run_take(&runtime, prompt, &voice) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("  take failed: {e}");
                    continue;
                }
            };

            write_wav(&out.join(format!("take{:02}.wav", idx + 1)), &take.audio).ok();

            let batch_started = Instant::now();
            let batch = {
                let c = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                    &take.audio,
                    SR,
                    "default",
                );
                let mut r = runtime.lock().unwrap();
                r.transcribe(&c).map_err(|e| format!("batch: {e}"))?
            };
            let batch_ms = batch_started.elapsed().as_millis();

            let truth = norm(prompt);
            let (be, bn) = wer(&truth, &norm(&batch));
            let (ie, _) = wer(&truth, &norm(&take.text));
            let denom = bn.max(1) as f64;
            let bw = be as f64 * 100.0 / denom;
            let iw = ie as f64 * 100.0 / denom;
            let (de, dn) = wer(&norm(&batch), &norm(&take.text));
            let dis = de as f64 * 100.0 / dn.max(1) as f64;

            eprintln!(
                "  {:.1}s win={} live={} bg={}ms batch={}ms tail={}ms | batch {:.2}% live {:.2}% d{:+.2} dis {:.2}%",
                take.secs, take.windows, take.committed_during_capture, take.background_ms,
                batch_ms, take.tail_ms, bw, iw, iw - bw, dis
            );

            rows.push(serde_json::json!({
                "take": idx + 1, "secs": take.secs, "windows": take.windows,
                "live_background_ms": take.background_ms, "live_tail_ms": take.tail_ms,
                "batch_ms": batch_ms,
                "prompt": prompt, "batch": batch, "live_incremental": take.text,
                "batch_wer": bw, "incremental_wer": iw, "disagreement": dis,
                "worker_committed_during_capture": take.committed_during_capture,
            }));
            std::fs::write(
                out.join("results.json"),
                serde_json::to_string_pretty(&rows).unwrap_or_default(),
            )
            .ok();
        }
        summarise(&rows, &out);
        Ok(())
    }

    struct Take {
        audio: Vec<f32>,
        text: String,
        secs: f64,
        windows: usize,
        background_ms: u128,
        tail_ms: u128,
        committed_during_capture: usize,
    }

    fn run_take(
        runtime: &Arc<Mutex<AsrRuntime>>,
        prompt: &str,
        voice: &str,
    ) -> Result<Take, String> {
        let mut session = rightkit_audio::capture::CaptureSession::start(SessionConfig {
            device_id: None,
            device_platform_id: None,
            target_rate: SR,
            target_channels: 1,
            target_dtype: OutDType::Float32,
            block_ms: 20,
        })
        .map_err(|e| format!("capture start: {e}"))?;

        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));

        let w_buf = buf.clone();
        let w_done = done.clone();
        let w_rt = runtime.clone();
        let worker = std::thread::spawn(move || {
            let mut text = String::new();
            let mut start = 0usize;
            let mut windows = 0usize;
            let mut background_ms = 0u128;
            let mut committed = 0usize;
            loop {
                let available = { w_buf.lock().unwrap().len() };
                if available >= start + WINDOW {
                    let live_now = !w_done.load(Ordering::SeqCst);
                    let slice: Vec<f32> = {
                        let b = w_buf.lock().unwrap();
                        b[start..start + WINDOW].to_vec()
                    };
                    let t0 = Instant::now();
                    let c = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                        &slice, SR, "default",
                    );
                    let cut =
                        WINDOW - PADDING + quiet_cut(&c[WINDOW - PADDING..]).unwrap_or(PADDING);
                    let piece = {
                        let mut r = w_rt.lock().unwrap();
                        r.transcribe(&c[..cut]).unwrap_or_default()
                    };
                    append_overlap(&mut text, &piece);
                    background_ms += t0.elapsed().as_millis();
                    start += cut.max(1);
                    windows += 1;
                    if live_now {
                        committed += 1;
                    }
                    continue;
                }
                if w_done.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            (text, start, windows, background_ms, committed)
        });

        let mut say = std::process::Command::new("/usr/bin/say")
            .args(["-v", voice, "-r", "170", prompt])
            .spawn()
            .map_err(|e| format!("say: {e}"))?;

        loop {
            if let Ok(chunk) = session.read_f32_blocking(SRU / 10, 200) {
                buf.lock().unwrap().extend_from_slice(&chunk);
            }
            match say.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(e) => return Err(format!("say wait: {e}")),
            }
        }
        let until = Instant::now() + std::time::Duration::from_millis(700);
        while Instant::now() < until {
            if let Ok(chunk) = session.read_f32_blocking(SRU / 10, 200) {
                buf.lock().unwrap().extend_from_slice(&chunk);
            }
        }
        if let Ok(rest) = session.read_f32(SRU * 2) {
            buf.lock().unwrap().extend_from_slice(&rest);
        }
        session.stop();
        done.store(true, Ordering::SeqCst);

        let (mut text, start, windows, background_ms, committed) =
            worker.join().map_err(|_| "worker panicked".to_string())?;

        let audio = buf.lock().unwrap().clone();
        let t0 = Instant::now();
        let tail_text = if start < audio.len() {
            let c = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                &audio[start..],
                SR,
                "default",
            );
            let mut r = runtime.lock().unwrap();
            r.transcribe(&c).unwrap_or_default()
        } else {
            String::new()
        };
        append_overlap(&mut text, &tail_text);
        let tail_ms = t0.elapsed().as_millis();

        Ok(Take {
            secs: audio.len() as f64 / SRU as f64,
            audio,
            text,
            windows,
            background_ms,
            tail_ms,
            committed_during_capture: committed,
        })
    }

    fn quiet_cut(tail: &[f32]) -> Option<usize> {
        const SPAN: usize = 200 * SRU / 1_000;
        const HOP: usize = 100 * SRU / 1_000;
        if tail.len() < SPAN {
            return None;
        }
        let mut best: Option<(f32, usize)> = None;
        let mut i = 0;
        while i + SPAN <= tail.len() {
            let e = rightkit_dictation_core::audio_conditioning::rms(&tail[i..i + SPAN]);
            if best.map(|(b, _)| e < b).unwrap_or(true) {
                best = Some((e, i + SPAN / 2));
            }
            i += HOP;
        }
        best.map(|(_, m)| m)
    }

    fn append_overlap(acc: &mut String, piece: &str) {
        let p = piece.trim();
        if p.is_empty() {
            return;
        }
        if acc.is_empty() {
            acc.push_str(p);
            return;
        }
        let a: Vec<char> = acc.chars().collect();
        let b: Vec<char> = p.chars().collect();
        let max = 16.min(a.len()).min(b.len());
        for n in (1..=max).rev() {
            if a[a.len() - n..] == b[..n] {
                acc.extend(b[n..].iter());
                return;
            }
        }
        acc.push(' ');
        acc.push_str(p);
    }

    fn write_wav(path: &std::path::Path, audio: &[f32]) -> Result<(), String> {
        rightkit_audio::wav::write_wav_mono_i16(path, SR, audio).map_err(|e| format!("{e}"))
    }

    fn norm(s: &str) -> Vec<String> {
        s.to_lowercase()
            .replace('%', " percent ")
            .replace('$', " dollars ")
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .map(|w| w.to_string())
            .collect()
    }

    fn wer(r: &[String], h: &[String]) -> (usize, usize) {
        let mut d = vec![vec![0usize; h.len() + 1]; r.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for (j, value) in d[0].iter_mut().enumerate() {
            *value = j;
        }
        for i in 1..=r.len() {
            for j in 1..=h.len() {
                let sub = d[i - 1][j - 1] + usize::from(r[i - 1] != h[j - 1]);
                d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
            }
        }
        (d[r.len()][h.len()], r.len())
    }

    fn summarise(rows: &[serde_json::Value], out: &std::path::Path) {
        if rows.is_empty() {
            eprintln!("no takes");
            return;
        }
        let n = rows.len() as f64;
        let avg = |k: &str| rows.iter().filter_map(|r| r[k].as_f64()).sum::<f64>() / n;
        let long = rows
            .iter()
            .filter(|r| r["secs"].as_f64().unwrap_or(0.0) >= 15.0)
            .count();
        let live_windows: u64 = rows
            .iter()
            .filter_map(|r| r["worker_committed_during_capture"].as_u64())
            .sum();
        let worse = rows
            .iter()
            .filter(|r| {
                r["incremental_wer"].as_f64().unwrap_or(0.0)
                    - r["batch_wer"].as_f64().unwrap_or(0.0)
                    > 1.0
            })
            .count();
        println!("\n═════════ LIVE INCREMENTAL A/B ═════════");
        println!("takes                     : {}", rows.len());
        println!("  >=15s (exercise live)   : {long}");
        println!("windows committed LIVE    : {live_windows}");
        println!("mean batch WER            : {:.2}%", avg("batch_wer"));
        println!("mean live-incremental WER : {:.2}%", avg("incremental_wer"));
        println!(
            "delta                     : {:+.2} points",
            avg("incremental_wer") - avg("batch_wer")
        );
        println!("mean disagreement         : {:.2}%", avg("disagreement"));
        println!("mean batch decode         : {:.0} ms", avg("batch_ms"));
        println!("mean after-stop tail      : {:.0} ms", avg("live_tail_ms"));
        println!("takes >1pt WORSE          : {worse}");
        println!("\nresults: {}", out.join("results.json").display());
    }
}

#[cfg(target_os = "macos")]
fn main() {
    if let Err(e) = live::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
