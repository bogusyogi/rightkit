//! Replay recorded takes through current shared control-tail policy.
//!
//! Uses rolling 3 s windows, 250 ms regular cadence, 60 ms armed-prefix cadence,
//! score-5 probe bias, exact immediate fire, fuzzy confirmation over 300 ms of
//! fresh audio, and 200/400 ms quiet-tail requirements. The file boundary is a
//! conservative silence anchor; each take receives 2 s of trailing silence.
//!
//! Prints every probe decode, parser decision, per-take verdict, and inference
//! latency evidence.
//!
//! Usage: tail_trigger_replay <models_dir> <wav> [<wav>...]

use std::path::Path;
use std::time::Instant;

use rightkit_dictation_engine::asr::{apply_probe_context_bias, AsrEp, AsrRuntime};
use rightkit_text_polish::text_pipeline::{
    has_trailing_control_wake, parse_control_command, ControlCommand, ControlIntent,
};

const SAMPLE_RATE: usize = 16_000;
const TAIL_WINDOW_MS: usize = 3_000;

fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != SAMPLE_RATE as u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            SAMPLE_RATE as u32,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

fn normalize_token(token: &str) -> String {
    token
        .trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
        .to_ascii_lowercase()
}

/// Mirror of the worker's private one-token split-prefix pairing.
fn pair_with_latched_prefix(text: &str) -> Option<ControlCommand> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() != 1 {
        return None;
    }
    let last = normalize_token(tokens.last()?);
    if !matches!(last.as_str(), "send" | "enter" | "stop" | "cancel") {
        return None;
    }
    parse_control_command(&format!("zephyr {last}"))
}

struct CurrentVerdict {
    fired_ms: Option<usize>,
    how: &'static str,
    probes: usize,
    probe_latencies_ms: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeDecision {
    Awaiting,
    FireExact,
    FireFuzzy,
    FireSplit,
}

#[derive(Default)]
struct CurrentPolicyState {
    pending_prefix_at_ms: Option<usize>,
    pending_fuzzy: Option<PendingFuzzy>,
}

#[derive(Clone, Copy)]
struct PendingFuzzy {
    intent: ControlIntent,
    total_samples: usize,
    at_ms: usize,
}

fn strong_fuzzy_control_verb(verb: &str) -> bool {
    matches!(
        normalize_token(verb).as_str(),
        "send"
            | "sent"
            | "sen"
            | "sand"
            | "said"
            | "says"
            | "say"
            | "sea"
            | "enter"
            | "entered"
            | "stop"
            | "stopped"
            | "stuff"
            | "stap"
            | "step"
            | "stock"
            | "stuck"
            | "cancel"
            | "cancelled"
            | "canceled"
            | "cansel"
            | "cancle"
    )
}

fn trailing_tokens_are(text: &str, wake: &str, verb: &str) -> bool {
    let mut tokens = text
        .split_whitespace()
        .rev()
        .map(normalize_token)
        .filter(|token| !token.is_empty());
    let (Some(last), Some(previous)) = (tokens.next(), tokens.next()) else {
        return false;
    };
    last == normalize_token(verb) && previous == normalize_token(wake)
}

fn exact_control_command(control: &ControlCommand, paired: bool, text: &str) -> bool {
    !paired
        && control.wake_word.eq_ignore_ascii_case("zephyr")
        && strong_fuzzy_control_verb(&control.verb)
        && trailing_tokens_are(text, &control.wake_word, &control.verb)
}

fn tail_text_ends_with_control_verb(text: &str) -> bool {
    text.split_whitespace()
        .last()
        .is_some_and(|last| parse_control_command(&format!("zephyr {last}")).is_some())
}

impl CurrentPolicyState {
    fn prefix_pending(&mut self, now_ms: usize) -> bool {
        if self
            .pending_prefix_at_ms
            .is_some_and(|at| now_ms.saturating_sub(at) > 1_500)
        {
            self.pending_prefix_at_ms = None;
        }
        self.pending_prefix_at_ms.is_some()
    }

    fn observe(
        &mut self,
        text: &str,
        total_samples: usize,
        now_ms: usize,
        probe_ms: u64,
        silence_ms: usize,
    ) -> ProbeDecision {
        const PREFIX_GRACE_MS: usize = 1_500;
        const FUZZY_NEW_AUDIO_SAMPLES: usize = SAMPLE_RATE * 3 / 10;

        if self
            .pending_prefix_at_ms
            .is_some_and(|at| now_ms.saturating_sub(at) > PREFIX_GRACE_MS)
        {
            self.pending_prefix_at_ms = None;
        }

        let mut paired = false;
        let control = parse_control_command(text.trim()).or_else(|| {
            let parsed =
                pair_with_latched_prefix(text).filter(|_| self.pending_prefix_at_ms.is_some());
            paired = parsed.is_some();
            parsed
        });
        let saw_candidate =
            has_trailing_control_wake(text.trim()) || tail_text_ends_with_control_verb(text.trim());

        if control.is_none() {
            self.pending_fuzzy = None;
            if saw_candidate {
                self.pending_prefix_at_ms.get_or_insert(now_ms);
            } else {
                self.pending_prefix_at_ms = None;
            }
            return ProbeDecision::Awaiting;
        }

        let control = control.expect("checked above");
        if exact_control_command(&control, paired, text) {
            self.pending_fuzzy = None;
            self.pending_prefix_at_ms = None;
            return ProbeDecision::FireExact;
        }

        let required_silence_ms = if paired || strong_fuzzy_control_verb(&control.verb) {
            200
        } else {
            400
        };
        let window_ms = 1_500usize.max((probe_ms as usize).saturating_mul(2));
        let confirmed = silence_ms >= required_silence_ms
            && self.pending_fuzzy.is_some_and(|prior| {
                prior.intent == control.intent
                    && total_samples >= prior.total_samples.saturating_add(FUZZY_NEW_AUDIO_SAMPLES)
                    && now_ms.saturating_sub(prior.at_ms) <= window_ms
            });
        if confirmed {
            self.pending_fuzzy = None;
            self.pending_prefix_at_ms = None;
            return if paired {
                ProbeDecision::FireSplit
            } else {
                ProbeDecision::FireFuzzy
            };
        }

        let replace = self.pending_fuzzy.is_none_or(|prior| {
            prior.intent != control.intent || now_ms.saturating_sub(prior.at_ms) > window_ms
        });
        if replace {
            self.pending_fuzzy = Some(PendingFuzzy {
                intent: control.intent,
                total_samples,
                at_ms: now_ms,
            });
        }
        self.pending_prefix_at_ms.get_or_insert(now_ms);
        ProbeDecision::Awaiting
    }
}

fn percentile(values: &[u64], percentile: f32) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f32 * percentile)
        .round()
        .clamp(0.0, (sorted.len() - 1) as f32) as usize;
    sorted[index]
}

fn replay_current(
    runtime: &mut AsrRuntime,
    audio: &[f32],
    original_samples: usize,
    tail_only: bool,
    verbose: bool,
) -> Result<CurrentVerdict, String> {
    const REGULAR_CADENCE_MS: usize = 250;
    const PREFIX_CADENCE_MS: usize = 60;
    const REGULAR_NEW_AUDIO_SAMPLES: usize = SAMPLE_RATE / 10;
    const PREFIX_NEW_AUDIO_SAMPLES: usize = SAMPLE_RATE / 20;

    let total_ms = audio.len() * 1_000 / SAMPLE_RATE;
    let mut policy = CurrentPolicyState::default();
    let replay_start_ms = if tail_only {
        (original_samples * 1_000 / SAMPLE_RATE).saturating_sub(TAIL_WINDOW_MS + REGULAR_CADENCE_MS)
    } else {
        0
    };
    let mut last_submit_ms = replay_start_ms / REGULAR_CADENCE_MS * REGULAR_CADENCE_MS;
    let mut last_submit_samples = last_submit_ms * SAMPLE_RATE / 1_000;
    let mut probes = 0usize;
    let mut latencies = Vec::new();

    loop {
        let prefix_pending = policy.prefix_pending(last_submit_ms);
        let cadence_ms = if prefix_pending {
            PREFIX_CADENCE_MS
        } else {
            REGULAR_CADENCE_MS
        };
        let fresh_samples = if prefix_pending {
            PREFIX_NEW_AUDIO_SAMPLES
        } else {
            REGULAR_NEW_AUDIO_SAMPLES
        };
        let time_due_ms = last_submit_ms.saturating_add(cadence_ms);
        let audio_due_ms = last_submit_samples
            .saturating_add(fresh_samples)
            .saturating_mul(1_000)
            / SAMPLE_RATE;
        let now_ms = time_due_ms.max(audio_due_ms);
        if now_ms > total_ms {
            break;
        }

        let end = (now_ms * SAMPLE_RATE / 1_000).min(audio.len());
        if end <= last_submit_samples {
            break;
        }
        let start = end.saturating_sub(TAIL_WINDOW_MS * SAMPLE_RATE / 1_000);
        let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
            &audio[start..end],
            SAMPLE_RATE as u32,
            "default",
        );
        let started = Instant::now();
        let text = runtime.transcribe(&conditioned)?;
        let probe_ms = started.elapsed().as_millis() as u64;
        probes += 1;
        latencies.push(probe_ms);
        let silence_ms = end.saturating_sub(original_samples).saturating_mul(1_000) / SAMPLE_RATE;
        let decision = policy.observe(&text, end, now_ms, probe_ms, silence_ms);
        if verbose {
            println!(
                "    t={now_ms:>5}ms [{start:>6}..{end:>6}] decode_ms={probe_ms:>4} silence_ms={silence_ms:>4} decision={decision:?} {:?}",
                text
            );
        }
        match decision {
            ProbeDecision::FireExact => {
                return Ok(CurrentVerdict {
                    fired_ms: Some(now_ms),
                    how: "exact",
                    probes,
                    probe_latencies_ms: latencies,
                });
            }
            ProbeDecision::FireFuzzy => {
                return Ok(CurrentVerdict {
                    fired_ms: Some(now_ms),
                    how: "fuzzy",
                    probes,
                    probe_latencies_ms: latencies,
                });
            }
            ProbeDecision::FireSplit => {
                return Ok(CurrentVerdict {
                    fired_ms: Some(now_ms),
                    how: "split-pair",
                    probes,
                    probe_latencies_ms: latencies,
                });
            }
            ProbeDecision::Awaiting => {}
        }
        last_submit_ms = now_ms;
        last_submit_samples = end;
    }

    Ok(CurrentVerdict {
        fired_ms: None,
        how: "miss",
        probes,
        probe_latencies_ms: latencies,
    })
}

#[cfg(test)]
mod current_policy_tests {
    use super::{CurrentPolicyState, ProbeDecision};

    #[test]
    fn canonical_pair_fires_on_first_probe() {
        let mut policy = CurrentPolicyState::default();
        assert_eq!(
            policy.observe("zephyr stop", 160_000, 10_000, 90, 0),
            ProbeDecision::FireExact
        );
    }

    #[test]
    fn fuzzy_pair_requires_later_audio_and_quiet_tail() {
        let mut policy = CurrentPolicyState::default();
        assert_eq!(
            policy.observe("zapper stop", 160_000, 10_000, 90, 0),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("zapper stop", 164_800, 10_300, 90, 200),
            ProbeDecision::FireFuzzy
        );
    }

    #[test]
    fn continued_speech_cancels_fuzzy_candidate() {
        let mut policy = CurrentPolicyState::default();
        assert_eq!(
            policy.observe("zapper stop", 160_000, 10_000, 90, 0),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("zapper stopped making noise", 164_800, 10_300, 90, 0),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("zapper stop", 169_600, 10_600, 90, 400),
            ProbeDecision::Awaiting
        );
    }

    #[test]
    fn split_prefix_requires_confirmed_single_token_tail() {
        let mut policy = CurrentPolicyState::default();
        assert_eq!(
            policy.observe("zephyr", 160_000, 10_000, 90, 0),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("stop", 164_800, 10_300, 90, 200),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("stop", 169_600, 10_600, 90, 500),
            ProbeDecision::FireSplit
        );
    }

    #[test]
    fn weak_verb_requires_four_hundred_ms_quiet_tail() {
        let mut policy = CurrentPolicyState::default();
        assert_eq!(
            policy.observe("zephyr and", 160_000, 10_000, 90, 0),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("zephyr and", 164_800, 10_300, 90, 200),
            ProbeDecision::Awaiting
        );
        assert_eq!(
            policy.observe("zephyr and", 169_600, 10_600, 90, 400),
            ProbeDecision::FireFuzzy
        );
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: tail_trigger_replay <models_dir> <wav> [<wav>...]".into());
    }
    let mut runtime = AsrRuntime::load(Path::new(&args[0]), AsrEp::resolve_default())?;
    let _ = runtime.transcribe(&vec![0.0; SAMPLE_RATE]);
    apply_probe_context_bias(&mut runtime);
    let tail_only = std::env::var("RIGHTKIT_REPLAY_TAIL_ONLY").ok().as_deref() == Some("1");

    let mut summary: Vec<(String, CurrentVerdict)> = Vec::new();
    let mut all_probe_latencies_ms = Vec::new();
    for wav in &args[1..] {
        let mut audio = load_wav(Path::new(wav))?;
        let original_samples = audio.len();
        audio.resize(audio.len() + SAMPLE_RATE * 2, 0.0);
        println!(
            "\n=== {wav} ({:.1}s)",
            audio.len() as f32 / SAMPLE_RATE as f32
        );
        println!("  -- current shared policy");
        let verdict = replay_current(&mut runtime, &audio, original_samples, tail_only, true)?;
        all_probe_latencies_ms.extend_from_slice(&verdict.probe_latencies_ms);
        summary.push((wav.clone(), verdict));
    }

    println!(
        "\n{:<40} {:>18} {:>14} {:>14}",
        "take", "current policy", "mean decode", "p95 decode"
    );
    for (wav, verdict) in &summary {
        let name = Path::new(wav)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| wav.clone());
        let result = match verdict.fired_ms {
            Some(ms) => format!("{} @{}ms/{}p", verdict.how, ms, verdict.probes),
            None => format!("MISS/{}p", verdict.probes),
        };
        let mean = if verdict.probe_latencies_ms.is_empty() {
            0.0
        } else {
            verdict.probe_latencies_ms.iter().sum::<u64>() as f64
                / verdict.probe_latencies_ms.len() as f64
        };
        println!(
            "{:<40} {:>18} {:>11.1} ms {:>11} ms",
            name,
            result,
            mean,
            percentile(&verdict.probe_latencies_ms, 0.95)
        );
    }
    let mean = if all_probe_latencies_ms.is_empty() {
        0.0
    } else {
        all_probe_latencies_ms.iter().sum::<u64>() as f64 / all_probe_latencies_ms.len() as f64
    };
    println!(
        "PROBE_LATENCY count={} mean_ms={:.1} p95_ms={}",
        all_probe_latencies_ms.len(),
        mean,
        percentile(&all_probe_latencies_ms, 0.95)
    );
    Ok(())
}
