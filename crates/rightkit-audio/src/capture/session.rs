// Capture session: cpal::Stream + lock-free ring buffer + resampler.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{anyhow, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use ringbuf::{
    traits::{Consumer, Observer, Producer, Split},
    HeapRb,
};

use super::device::resolve_device;
use super::downmix::{push_mono_f32, push_mono_pcm, MonoDownmix};
use super::metrics::{CaptureErrorKind, Metrics, MetricsSnapshot};
use super::resample::StreamResampler;
use super::staging::{pending_resampled_len, take_resampled};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutDType {
    Int16,
    Float32,
}

impl OutDType {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "int16" | "i16" => Ok(OutDType::Int16),
            "float32" | "f32" => Ok(OutDType::Float32),
            other => Err(anyhow!(
                "unsupported dtype '{other}', want int16 or float32"
            )),
        }
    }
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            OutDType::Int16 => 2,
            OutDType::Float32 => 4,
        }
    }
}

pub struct SessionConfig {
    pub device_id: Option<usize>,
    /// Stable platform identity captured during device selection. When set,
    /// capture resolves this exact device and fails closed if unavailable.
    pub device_platform_id: Option<String>,
    pub target_rate: u32,
    pub target_channels: u16, // currently must be 1
    pub target_dtype: OutDType,
    pub block_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureInputWaitStatus {
    NoCallbacks,
    NoSamples,
    Ready,
}

fn classify_input_wait(
    callbacks_before: u64,
    callbacks_after: u64,
    output_samples: usize,
) -> CaptureInputWaitStatus {
    if output_samples > 0 {
        CaptureInputWaitStatus::Ready
    } else if callbacks_after == callbacks_before {
        CaptureInputWaitStatus::NoCallbacks
    } else {
        CaptureInputWaitStatus::NoSamples
    }
}

// Touch every ring page before the callback owns producer storage, then leave
// ring empty for normal capture.
fn prewarm_ring_storage(
    producer: &mut ringbuf::HeapProd<f32>,
    consumer: &mut ringbuf::HeapCons<f32>,
    capacity: usize,
) {
    let warmed = producer.push_iter(std::iter::repeat_n(0.0, capacity));
    debug_assert_eq!(warmed, capacity);
    consumer.clear();
}

#[derive(Debug)]
pub enum CaptureFirstBuffer {
    Data(Vec<f32>),
    NoCallbacks,
    NoSamples,
    StreamError(CaptureErrorKind),
}

#[derive(Default)]
struct AudioArrival {
    epoch: AtomicU64,
    mutex: Mutex<()>,
    ready: Condvar,
}

impl AudioArrival {
    fn signal(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
        self.ready.notify_one();
    }

    fn wait(&self, seen: &mut u64, timeout: Duration) -> bool {
        let current = self.epoch.load(Ordering::Acquire);
        if current != *seen {
            *seen = current;
            return true;
        }
        let guard = self
            .mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = self
            .ready
            .wait_timeout_while(guard, timeout, |_| {
                self.epoch.load(Ordering::Acquire) == *seen
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(guard);
        let current = self.epoch.load(Ordering::Acquire);
        let changed = current != *seen;
        *seen = current;
        changed
    }
}

/// What the Python side gets back as a handle. Internally just an index.
pub struct CaptureSession {
    /// cpal stream — keep alive for the lifetime of the session.
    _stream: cpal::Stream,
    consumer: ringbuf::HeapCons<f32>,
    resampler: StreamResampler,
    metrics: Arc<Metrics>,
    target_rate: u32,
    target_dtype: OutDType,
    input_rate: u32,
    input_channels: u16,
    input_sample_format: String,
    stopped: AtomicBool,
    audio_arrival: Arc<AudioArrival>,
    arrival_seen: u64,
    /// Resampled-output staging buffer. `out_buf_start` advances without
    /// front-draining on every read; occasional compaction is amortized.
    out_buf_f32: Vec<f32>,
    out_buf_start: usize,
    /// Scratch buffer for native-rate samples drained from the ring before
    /// resampling. Reused across `drain_resampled` / `read_f32_blocking` /
    /// `read_blocking` calls (cleared, not reallocated) so the hot read path
    /// — invoked every `block_ms` during a recording — does not allocate a
    /// fresh `Vec` per call. (Perf audit: per-call allocations in
    /// drain_resampled/read_blocking.)
    native_scratch: Vec<f32>,
}

impl CaptureSession {
    /// Build capture stream without starting device playback.
    ///
    /// Caller can flush/setup session while paused, then call `resume()` once
    /// playback should begin.
    pub fn open_paused(cfg: SessionConfig) -> Result<Self> {
        if cfg.target_channels != 1 {
            return Err(anyhow!(
                "only mono output is supported (got target_channels={})",
                cfg.target_channels
            ));
        }
        if cfg.target_rate == 0 {
            return Err(anyhow!("target_rate must be > 0"));
        }

        let device = resolve_device(cfg.device_id, cfg.device_platform_id.as_deref())?;
        let device_name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());

        let supported = device
            .default_input_config()
            .map_err(|e| anyhow!("default_input_config() failed for '{device_name}': {e}"))?;
        let input_rate = supported.sample_rate();
        let input_channels = supported.channels();
        let sample_format = supported.sample_format();

        let stream_config: StreamConfig = supported.into();

        tracing::info!(
            device = %device_name,
            input_rate,
            input_channels,
            ?sample_format,
            target_rate = cfg.target_rate,
            "starting capture session"
        );

        // Ring buffer holds 4 seconds of input-rate mono f32 — generous
        // headroom to absorb consumer-side stalls without dropping.
        let ring_capacity = (input_rate as usize) * 4;
        let rb = HeapRb::<f32>::new(ring_capacity);
        let (mut producer, mut consumer) = rb.split();
        prewarm_ring_storage(&mut producer, &mut consumer, ring_capacity);

        let metrics = Arc::new(Metrics::new());
        let metrics_cb = Arc::clone(&metrics);
        let metrics_err = Arc::clone(&metrics);
        let audio_arrival = Arc::new(AudioArrival::default());
        let audio_arrival_cb = Arc::clone(&audio_arrival);

        let in_chans = input_channels as usize;
        let mut downmix = MonoDownmix::new(in_chans, input_rate);

        let err_fn = move |e: cpal::Error| {
            metrics_err.record_async_error(CaptureErrorKind::from_cpal(e.kind()));
        };

        macro_rules! build_pcm_stream {
            ($sample:ty) => {
                device.build_input_stream(
                    stream_config,
                    move |data: &[$sample], _info| {
                        let t0 = Instant::now();
                        push_mono_pcm(data, in_chans, &mut downmix, &mut producer, &metrics_cb);
                        audio_arrival_cb.signal();
                        let dur = t0.elapsed().as_micros() as u64;
                        metrics_cb.record_callback(dur, (data.len() / in_chans.max(1)) as u64);
                    },
                    err_fn,
                    None,
                )
            };
        }

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                stream_config,
                move |data: &[f32], _info| {
                    let t0 = Instant::now();
                    push_mono_f32(data, in_chans, &mut downmix, &mut producer, &metrics_cb);
                    audio_arrival_cb.signal();
                    let dur = t0.elapsed().as_micros() as u64;
                    metrics_cb.record_callback(dur, (data.len() / in_chans.max(1)) as u64);
                },
                err_fn,
                None,
            ),
            SampleFormat::I8 => build_pcm_stream!(i8),
            SampleFormat::I16 => build_pcm_stream!(i16),
            SampleFormat::I24 => build_pcm_stream!(cpal::I24),
            SampleFormat::I32 => build_pcm_stream!(i32),
            SampleFormat::I64 => build_pcm_stream!(i64),
            SampleFormat::U8 => build_pcm_stream!(u8),
            SampleFormat::U16 => build_pcm_stream!(u16),
            SampleFormat::U24 => build_pcm_stream!(cpal::U24),
            SampleFormat::U32 => build_pcm_stream!(u32),
            SampleFormat::U64 => build_pcm_stream!(u64),
            SampleFormat::F64 => build_pcm_stream!(f64),
            other => {
                return Err(anyhow!(
                    "unsupported sample format from device '{device_name}': {:?}",
                    other
                ))
            }
        }
        .map_err(|e| anyhow!("build_input_stream failed: {e}"))?;

        let resampler = StreamResampler::new(input_rate, cfg.target_rate)?;

        Ok(Self {
            _stream: stream,
            consumer,
            resampler,
            metrics,
            target_rate: cfg.target_rate,
            target_dtype: cfg.target_dtype,
            input_rate,
            input_channels,
            input_sample_format: format!("{sample_format:?}"),
            stopped: AtomicBool::new(false),
            audio_arrival,
            arrival_seen: 0,
            out_buf_f32: Vec::with_capacity(cfg.target_rate as usize),
            out_buf_start: 0,
            native_scratch: Vec::with_capacity(ring_capacity.min(cfg.target_rate as usize * 4)),
        })
    }

    pub fn start(cfg: SessionConfig) -> Result<Self> {
        let session = Self::open_paused(cfg)?;
        // Start latency is measured from the native play call, not stream
        // construction; resume() applies same reset for warm starts.
        session.metrics.reset_capture_epoch();
        session
            ._stream
            .play()
            .map_err(|e| anyhow!("stream.play() failed: {e}"))?;
        Ok(session)
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }
    pub fn input_channels(&self) -> u16 {
        self.input_channels
    }
    pub fn input_sample_format(&self) -> &str {
        &self.input_sample_format
    }
    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }
    pub fn target_dtype(&self) -> OutDType {
        self.target_dtype
    }

    /// Drain everything currently buffered in the ring at native rate into
    /// `native_scratch` (cleared first), then resample it into `out_buf_f32`.
    /// Shared by every read path that needs a fresh top-up before taking from
    /// the resampled staging buffer.
    fn drain_native_into_scratch_and_resample(&mut self) -> Result<()> {
        self.native_scratch.clear();
        let mut tmp = [0f32; 4096];
        loop {
            let n = self.consumer.pop_slice(&mut tmp);
            if n == 0 {
                break;
            }
            self.native_scratch.extend_from_slice(&tmp[..n]);
            if n < tmp.len() {
                break;
            }
        }
        if !self.native_scratch.is_empty() {
            self.resampler
                .process(&self.native_scratch, &mut self.out_buf_f32)?;
        }
        Ok(())
    }

    /// Drain all pending ring samples, resample, and return up to `max_samples`
    /// of target-rate f32 output. This is what `read_f32` returns directly.
    fn drain_resampled(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        self.ensure_stream_healthy()?;
        self.drain_native_into_scratch_and_resample()?;
        let chunk = take_resampled(&mut self.out_buf_f32, &mut self.out_buf_start, max_samples);
        self.metrics.add_emitted(chunk.len() as u64);
        self.ensure_stream_healthy()?;
        Ok(chunk)
    }

    /// Drain available resampled output as f32 samples (target rate, mono),
    /// skipping the dtype byte-encode. This is what the sidecar ASR worker
    /// consumes — the decoder wants `&[f32]`, so routing through the
    /// int16/float32 byte encoding (the Python ABI) would be pure waste.
    pub fn read_f32(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        self.drain_resampled(max_samples)
    }

    /// Wait for a native callback without polling. Callback code only bumps an
    /// epoch and signals; it never takes this mutex.
    pub fn wait_for_audio(&mut self, timeout: std::time::Duration) -> bool {
        if self.consumer.occupied_len() > 0
            || pending_resampled_len(&self.out_buf_f32, self.out_buf_start) > 0
        {
            return true;
        }
        self.audio_arrival.wait(&mut self.arrival_seen, timeout)
    }

    /// Blocking f32 variant used by sidecar ASR worker to seed recording after
    /// `stream.play()`. Runtime opens & pauses stream during engine startup;
    /// this waits briefly for first resumed callback frames so leading phonemes
    /// are less likely to be dropped.
    pub fn read_f32_blocking(&mut self, min_samples: usize, timeout_ms: u64) -> Result<Vec<f32>> {
        let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            self.ensure_stream_healthy()?;
            self.drain_native_into_scratch_and_resample()?;
            if pending_resampled_len(&self.out_buf_f32, self.out_buf_start) >= min_samples
                || Instant::now() >= deadline
            {
                break;
            }
            self.wait_for_audio(deadline.saturating_duration_since(Instant::now()));
        }

        let pending = pending_resampled_len(&self.out_buf_f32, self.out_buf_start);
        let take = if pending >= min_samples {
            pending.min(min_samples * 2)
        } else {
            pending
        };
        let chunk = take_resampled(&mut self.out_buf_f32, &mut self.out_buf_start, take);
        self.metrics.add_emitted(chunk.len() as u64);
        self.ensure_stream_healthy()?;
        Ok(chunk)
    }

    pub fn wait_first_buffer(
        &mut self,
        min_samples: usize,
        timeout_ms: u64,
    ) -> Result<CaptureFirstBuffer> {
        let before = self.metrics.snapshot();
        let samples = match self.read_f32_blocking(min_samples, timeout_ms) {
            Ok(samples) => samples,
            Err(err) => {
                if let Some(kind) = self.metrics.snapshot().terminal_error_kind {
                    return Ok(CaptureFirstBuffer::StreamError(kind));
                }
                return Err(err);
            }
        };
        let after = self.metrics.snapshot();
        if let Some(kind) = after.terminal_error_kind {
            return Ok(CaptureFirstBuffer::StreamError(kind));
        }
        Ok(
            match classify_input_wait(
                before.callback_invocations,
                after.callback_invocations,
                samples.len(),
            ) {
                CaptureInputWaitStatus::Ready => CaptureFirstBuffer::Data(samples),
                CaptureInputWaitStatus::NoCallbacks => CaptureFirstBuffer::NoCallbacks,
                CaptureInputWaitStatus::NoSamples => CaptureFirstBuffer::NoSamples,
            },
        )
    }

    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    fn ensure_stream_healthy(&self) -> Result<()> {
        if let Some(kind) = self.metrics.snapshot().terminal_error_kind {
            Err(anyhow!("capture stream error: {}", kind.as_str()))
        } else {
            Ok(())
        }
    }

    /// Discard everything currently buffered in the capture pipeline so the NEXT
    /// recording starts from silence: drain+drop the ring, clear the resampled
    /// staging buffer, and reset the resampler's leftover + filter state. Without
    /// this, a fast back-to-back recording prepends a stale prefix — the prior
    /// clip's tail plus whatever the still-warm stream pushed into the ring during
    /// the idle gap — to the new clip, which collapses the transcript to empty.
    /// Returns the number of native-rate input samples discarded (0 on a clean
    /// stop; larger when the ring kept filling between recordings).
    pub fn flush(&mut self) -> usize {
        let mut discarded = 0usize;
        let mut tmp = [0f32; 4096];
        loop {
            let n = self.consumer.pop_slice(&mut tmp);
            if n == 0 {
                break;
            }
            discarded += n;
        }
        self.out_buf_f32.clear();
        self.out_buf_start = 0;
        self.resampler.reset();
        discarded
    }

    /// Pause the audio unit while keeping the device + stream OPEN. Stops capture
    /// (so the OS "mic in use" indicator turns off) without the cost of tearing the
    /// stream down; `resume()` restarts it cheaply. Best-effort.
    pub fn pause(&self) {
        let _ = self._stream.pause();
    }

    /// Resume capture on the already-open stream after `pause()`. Far cheaper than
    /// `start()` (no device enumeration / stream build).
    pub fn resume(&self) -> Result<()> {
        self.metrics.reset_capture_epoch();
        self._stream
            .play()
            .map_err(|e| anyhow!("stream.play() on resume failed: {e}"))
    }

    pub fn stop(&mut self) -> MetricsSnapshot {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // Dropping _stream on stop() would require consuming self; the
            // cpal stream will instead drop with the session struct.
            // Pause is best-effort.
            let _ = self._stream.pause();
        }
        self.metrics.snapshot()
    }
}
