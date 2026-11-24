//! Speech out: say an answer aloud, one utterance at a time, each with its own stop.
//!
//! [`Speaker`] takes text (the `text` of a Pulse answer written with `reply: "speech"`), splits it into
//! sentences, synthesises them one at a time and queues each to the audio sink as soon as it is ready, so
//! the first sentence is audible while the rest is still being made. Every [`speak`](Speaker::speak)
//! returns an [`UtteranceId`]; [`stop`](Speaker::stop) ends that utterance at once, whether it is
//! playing or still waiting, and leaves the others. [`stop_all`](Speaker::stop_all) is the barge-in: it
//! returns after the audio is flushed, so capture can start without the agent's voice in it.
//!
//! The synthesiser and the sink are traits. Adapters for the Qwen worker (`client`), the OS voice
//! (`system`) and a cpal output (`playback`) are below. Nothing here opens a microphone.
//!
//! Measured, not assumed: [`SpeechEvent::FirstAudio`] gives the time from `speak` to the first samples
//! queued. Device latency (`OutputControl::latency`) comes on top. Neither has been measured on the
//! Mac or the Dell.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Longest sentence chunk handed to the synthesiser, in characters.
const MAX_CHUNK_CHARS: usize = 220;
/// How long the speaking thread waits for room in a full sink, or for playout to finish.
const POLL: Duration = Duration::from_millis(10);

/// Names one utterance for its stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UtteranceId(pub u64);

/// Mono audio from a synthesiser.
#[derive(Debug, Clone)]
pub struct SpeechAudio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

/// Makes audio for one sentence. Runs on the speaking thread.
pub trait SpeechSynth {
    fn synthesize(&mut self, text: &str) -> Result<SpeechAudio, String>;
}

/// Where audio goes. Runs on the speaking thread, so it need not be `Send`.
pub trait SpeechSink {
    /// Rate of the mono samples [`push`](Self::push) takes.
    fn sample_rate(&self) -> u32;
    /// Queue mono samples; returns how many were accepted (the rest are refused because the queue is
    /// full and are offered again).
    fn push(&mut self, samples: &[f32]) -> usize;
    /// Mono samples queued and not yet played.
    fn queued(&self) -> usize;
    /// Periodic upkeep (device recovery, idle release). Called while the thread waits.
    fn service(&mut self) {}
}

/// What other threads may call to stop at once. Both must be safe to call while the speaking thread is
/// inside `synthesize` or `push`.
#[derive(Clone)]
pub struct StopHooks {
    /// Interrupt a synthesis in flight (the Qwen worker's `cancel_active`); a no-op where there is none.
    pub cancel_synthesis: Arc<dyn Fn() + Send + Sync>,
    /// Drop everything queued in the sink (`OutputControl::flush`).
    pub flush_audio: Arc<dyn Fn() + Send + Sync>,
}

/// What a [`Speaker`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEvent {
    /// The first samples of the utterance were queued, `after_ms` after `speak`.
    FirstAudio {
        id: UtteranceId,
        after_ms: u64,
    },
    /// All of it was played.
    Finished {
        id: UtteranceId,
    },
    /// It was stopped, playing or waiting.
    Stopped {
        id: UtteranceId,
    },
    Failed {
        id: UtteranceId,
        message: String,
    },
}

/// The pieces a [`Speaker`] needs, built on the speaking thread (an audio output is often not `Send`).
pub struct SpeechEngine {
    pub synth: Box<dyn SpeechSynth>,
    pub sink: Box<dyn SpeechSink>,
    pub hooks: StopHooks,
}

struct Job {
    id: UtteranceId,
    text: String,
    submitted: Instant,
}

#[derive(Default)]
struct State {
    active: Option<UtteranceId>,
    stopped: HashSet<u64>,
    /// Every id below this is stopped (`stop_all`).
    floor: u64,
}

impl State {
    fn is_stopped(&self, id: UtteranceId) -> bool {
        id.0 < self.floor || self.stopped.contains(&id.0)
    }
}

/// Speaks utterances in order on one thread.
pub struct Speaker {
    jobs: Sender<Job>,
    state: Arc<Mutex<State>>,
    hooks: StopHooks,
    next_id: AtomicU64,
}

impl Speaker {
    /// Start the speaking thread. `build` runs on that thread and makes the synthesiser, sink and stop
    /// hooks; its error is returned here. Events go to `events`.
    pub fn start<F>(build: F, events: Sender<SpeechEvent>) -> Result<Self, String>
    where
        F: FnOnce() -> Result<SpeechEngine, String> + Send + 'static,
    {
        let (jobs, job_rx) = channel::<Job>();
        let (ready_tx, ready_rx) = channel::<Result<StopHooks, String>>();
        let state = Arc::new(Mutex::new(State::default()));
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("rightkit-speech-out".to_string())
            .spawn(move || match build() {
                Ok(engine) => {
                    let _ = ready_tx.send(Ok(engine.hooks.clone()));
                    run(engine, &thread_state, &job_rx, &events);
                }
                Err(message) => {
                    let _ = ready_tx.send(Err(message));
                }
            })
            .map_err(|error| format!("speech thread: {error}"))?;
        let hooks = ready_rx
            .recv()
            .map_err(|_| "speech thread ended before it was ready".to_string())??;
        Ok(Self {
            jobs,
            state,
            hooks,
            next_id: AtomicU64::new(1),
        })
    }

    /// Queue `text` behind what is already being said. Returns the id to stop it with.
    pub fn speak(&self, text: impl Into<String>) -> UtteranceId {
        let id = UtteranceId(self.next_id.fetch_add(1, Ordering::SeqCst));
        let _ = self.jobs.send(Job {
            id,
            text: text.into(),
            submitted: Instant::now(),
        });
        id
    }

    /// Stop one utterance at once: if it is playing, synthesis is interrupted and the queued audio
    /// dropped before this returns; if it is waiting, it will not start. Others are untouched. Stopping
    /// speech does not cancel the task whose answer it was.
    pub fn stop(&self, id: UtteranceId) {
        let was_active = match self.state.lock() {
            Ok(mut state) => {
                state.stopped.insert(id.0);
                state.active == Some(id)
            }
            Err(_) => true,
        };
        if was_active {
            self.cut();
        }
    }

    /// Stop everything, playing and waiting: the barge-in. Returns after the audio is flushed.
    pub fn stop_all(&self) {
        let any_active = match self.state.lock() {
            Ok(mut state) => {
                state.floor = self.next_id.load(Ordering::SeqCst);
                state.stopped.clear();
                state.active.is_some()
            }
            Err(_) => true,
        };
        if any_active {
            self.cut();
        }
    }

    /// Whether an utterance is being said now.
    pub fn is_speaking(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.active.is_some())
            .unwrap_or(false)
    }

    fn cut(&self) {
        (self.hooks.cancel_synthesis)();
        (self.hooks.flush_audio)();
    }
}

enum Outcome {
    Finished,
    Stopped,
    Failed(String),
}

fn run(
    mut engine: SpeechEngine,
    state: &Arc<Mutex<State>>,
    jobs: &Receiver<Job>,
    events: &Sender<SpeechEvent>,
) {
    while let Ok(job) = jobs.recv() {
        let id = job.id;
        let stopped = |state: &Arc<Mutex<State>>| {
            state
                .lock()
                .map(|state| state.is_stopped(id))
                .unwrap_or(true)
        };
        let outcome = if stopped(state) {
            Outcome::Stopped
        } else {
            if let Ok(mut state) = state.lock() {
                state.active = Some(id);
            }
            let outcome = speak_one(&mut engine, &job, &|| stopped(state), events);
            if let Ok(mut state) = state.lock() {
                state.active = None;
            }
            outcome
        };
        if matches!(outcome, Outcome::Stopped) {
            // A push that raced the stop's flush must not play.
            (engine.hooks.flush_audio)();
        }
        if let Ok(mut state) = state.lock() {
            state.stopped.remove(&id.0);
        }
        let event = match outcome {
            Outcome::Finished => SpeechEvent::Finished { id },
            Outcome::Stopped => SpeechEvent::Stopped { id },
            Outcome::Failed(message) => SpeechEvent::Failed { id, message },
        };
        let _ = events.send(event);
    }
}

fn speak_one(
    engine: &mut SpeechEngine,
    job: &Job,
    stopped: &dyn Fn() -> bool,
    events: &Sender<SpeechEvent>,
) -> Outcome {
    let rate = engine.sink.sample_rate();
    let mut first_audio = true;
    for sentence in split_sentences(&job.text, MAX_CHUNK_CHARS) {
        if stopped() {
            return Outcome::Stopped;
        }
        let audio = match engine.synth.synthesize(&sentence) {
            Ok(audio) => audio,
            Err(_) if stopped() => return Outcome::Stopped,
            Err(message) => return Outcome::Failed(message),
        };
        let mono = resample_linear(&audio.samples, audio.sample_rate, rate);
        let mut offset = 0;
        while offset < mono.len() {
            if stopped() {
                return Outcome::Stopped;
            }
            let accepted = engine.sink.push(&mono[offset..]);
            if accepted > 0 && first_audio {
                first_audio = false;
                let _ = events.send(SpeechEvent::FirstAudio {
                    id: job.id,
                    after_ms: job.submitted.elapsed().as_millis() as u64,
                });
            }
            offset += accepted;
            if accepted == 0 {
                engine.sink.service();
                thread::sleep(POLL);
            }
        }
    }
    while engine.sink.queued() > 0 {
        if stopped() {
            return Outcome::Stopped;
        }
        engine.sink.service();
        thread::sleep(POLL);
    }
    Outcome::Finished
}

/// Split `text` into sentences (`.`, `!`, `?` or a line break ends one), breaking a long one at a space
/// once it passes `max_chars`. Empty pieces are dropped.
pub fn split_sentences(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(20);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            push_trimmed(&mut out, &mut current);
            continue;
        }
        current.push(c);
        let sentence_end =
            matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|next| next.is_whitespace());
        let too_long = c == ' ' && current.chars().count() >= max_chars;
        if sentence_end || too_long {
            push_trimmed(&mut out, &mut current);
        }
    }
    push_trimmed(&mut out, &mut current);
    out
}

fn push_trimmed(out: &mut Vec<String>, current: &mut String) {
    let piece = current.trim();
    if !piece.is_empty() {
        out.push(piece.to_string());
    }
    current.clear();
}

/// Linear-interpolation resample of mono audio. Speech-grade, not music-grade.
pub fn resample_linear(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || to == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    let step = f64::from(from) / f64::from(to);
    (0..out_len)
        .map(|i| {
            let position = i as f64 * step;
            let index = position as usize;
            let fraction = (position - index as f64) as f32;
            let a = samples[index.min(samples.len() - 1)];
            let b = samples[(index + 1).min(samples.len() - 1)];
            a + (b - a) * fraction
        })
        .collect()
}

fn pcm16_wav_to_audio(wav: &[u8]) -> Result<SpeechAudio, String> {
    let (sample_rate, pcm) =
        crate::wav::decode_pcm16(wav).ok_or_else(|| "expected a mono PCM16 WAV".to_string())?;
    if pcm.is_empty() || sample_rate == 0 {
        return Err("empty audio".to_string());
    }
    Ok(SpeechAudio {
        sample_rate,
        samples: pcm
            .iter()
            .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
            .collect(),
    })
}

/// The Qwen3-TTS worker as a synthesiser.
#[cfg(feature = "client")]
pub struct QtsSynth {
    client: crate::client::QtsClient,
    options: crate::client::SynthOptions,
}

#[cfg(feature = "client")]
impl QtsSynth {
    pub fn new(client: crate::client::QtsClient, options: crate::client::SynthOptions) -> Self {
        Self { client, options }
    }

    /// The hook that interrupts a synthesis in flight; pass it as [`StopHooks::cancel_synthesis`].
    pub fn cancel_hook() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(crate::client::cancel_active)
    }
}

#[cfg(feature = "client")]
impl SpeechSynth for QtsSynth {
    fn synthesize(&mut self, text: &str) -> Result<SpeechAudio, String> {
        let result = self
            .client
            .synthesize(text, &self.options)
            .map_err(|error| error.to_string())?;
        pcm16_wav_to_audio(&result.wav)
    }
}

/// The OS voice (`say`, SAPI) as a synthesiser: no model, no download.
#[cfg(feature = "system")]
pub struct SystemSynth {
    options: crate::system::SystemTtsOptions,
}

#[cfg(feature = "system")]
impl SystemSynth {
    pub fn new(options: crate::system::SystemTtsOptions) -> Self {
        Self { options }
    }
}

#[cfg(feature = "system")]
impl SpeechSynth for SystemSynth {
    fn synthesize(&mut self, text: &str) -> Result<SpeechAudio, String> {
        let result = crate::system::SystemTts
            .synthesize(text, &self.options)
            .map_err(|error| error.to_string())?;
        pcm16_wav_to_audio(&result.wav)
    }
}

/// A cpal output as the sink. Every sample it queues is also written to the echo reference
/// (`rightkit_audio::echo::reference_tap`), so capture in the same process can take the agent's voice
/// out of the microphone.
#[cfg(feature = "playback")]
pub struct OutputSink {
    session: rightkit_audio::playback::OutputSession,
    rate: u32,
    channels: usize,
    capacity_samples: usize,
}

#[cfg(feature = "playback")]
impl OutputSink {
    /// Open `device` (`None` = the default output).
    pub fn open(device: Option<&str>) -> Result<Self, String> {
        let session = rightkit_audio::playback::OutputSession::open(device)
            .map_err(|error| error.to_string())?;
        let format = session.format();
        Ok(Self {
            rate: format.sample_rate,
            channels: usize::from(format.channels.max(1)),
            capacity_samples: format.capacity_samples,
            session,
        })
    }

    /// The hook that drops everything queued; pass it as [`StopHooks::flush_audio`].
    pub fn flush_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        let control = self.session.control();
        Arc::new(move || control.flush())
    }
}

#[cfg(feature = "playback")]
impl SpeechSink for OutputSink {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn push(&mut self, samples: &[f32]) -> usize {
        let mut interleaved = Vec::with_capacity(samples.len() * self.channels);
        for sample in samples {
            interleaved.extend(std::iter::repeat_n(*sample, self.channels));
        }
        let accepted = self.session.push_samples(&interleaved) / self.channels;
        if accepted > 0 {
            let reference = resample_linear(
                &samples[..accepted],
                self.rate,
                rightkit_audio::echo::REFERENCE_RATE,
            );
            rightkit_audio::echo::reference_tap().push(&reference);
        }
        accepted
    }

    fn queued(&self) -> usize {
        self.capacity_samples
            .saturating_sub(self.session.free_samples())
            / self.channels
    }

    fn service(&mut self) {
        self.session.service();
    }
}
