// Capture session: cpal::Stream + lock-free ring buffer + resampler.

#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicU8;
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
use super::precise::{
    CallbackStamp, CaptureCutHandle, CaptureCutToken, PreciseEpoch, ProducerGate,
};
use super::resample::StreamResampler;
use super::staging::{pending_resampled_len, take_resampled};

// Only this module owns MMCSS handles. Keeping them in TLS (rather than in the
// callback closure) guarantees same-thread revert even if a backend drops its
// callback closure on the session owner. CPAL WASAPI uses one dedicated thread
// per stream, keeps it through pause/resume, then terminates & joins on Drop.
#[cfg(target_os = "windows")]
struct CaptureThreadMmcss {
    handle: Option<windows::Win32::Foundation::HANDLE>,
    // 1 = Pro Audio, 2 = Audio, 3 = both registration attempts refused.
    outcome: u8,
}

#[cfg(target_os = "windows")]
impl CaptureThreadMmcss {
    fn register() -> Self {
        use windows::core::w;
        use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

        let mut task_index = 0u32;
        // SAFETY: static NUL-terminated task name & valid writable task index;
        // registration is performed by the callback thread itself.
        if let Ok(handle) =
            unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }
        {
            return Self {
                handle: Some(handle),
                outcome: 1,
            };
        }
        let mut task_index = 0u32;
        // SAFETY: same as above; the failed first call created no registration.
        match unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) } {
            Ok(handle) => Self {
                handle: Some(handle),
                outcome: 2,
            },
            Err(_) => Self {
                handle: None,
                outcome: 3,
            },
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for CaptureThreadMmcss {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: this TLS guard retained the successful registration & is
            // destroyed on its registering thread. Revert releases the handle;
            // CloseHandle must not be used for an MMCSS registration.
            let _ = unsafe {
                windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle)
            };
        }
    }
}

#[cfg(target_os = "windows")]
fn join_capture_thread_mmcss(attempted: &mut bool, outcome: &AtomicU8) {
    if *attempted {
        return;
    }
    *attempted = true;
    thread_local! {
        static REGISTRATION: std::cell::OnceCell<CaptureThreadMmcss> = const {
            std::cell::OnceCell::new()
        };
    }
    let status = REGISTRATION.with(|registration| {
        registration
            .get_or_init(CaptureThreadMmcss::register)
            .outcome
    });
    // Only numeric telemetry crosses into the consumer; no logging, formatting,
    // locks or notifications are added to the data callback.
    outcome.store(status, Ordering::Relaxed);
}

#[cfg(target_os = "windows")]
fn report_capture_thread_mmcss(outcome: &AtomicU8) {
    let status = outcome.load(Ordering::Relaxed);
    // Bit 7 marks an already-reported result. Leave pending (0) untouched so a
    // consumer read before the first callback cannot consume the eventual result.
    if !(1..=3).contains(&status)
        || outcome
            .compare_exchange(status, status | 0x80, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    tracing::info!(
        event = "capture_thread_mmcss",
        outcome = status,
        class = match status {
            1 => "Pro Audio",
            2 => "Audio",
            _ => "none",
        },
        joined = status != 3,
        "capture callback thread MMCSS"
    );
}

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
    // The count is not needed: the ring is cleared right after; pushing is what touches the pages.
    let _ = producer.push_iter(std::iter::repeat_n(0.0, capacity));
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
    _stream: Arc<cpal::Stream>,
    producer_gate: Arc<ProducerGate>,
    stamp_consumer: ringbuf::HeapCons<CallbackStamp>,
    precise: Option<PreciseEpoch>,
    consumer: ringbuf::HeapCons<f32>,
    resampler: StreamResampler,
    metrics: Arc<Metrics>,
    #[cfg(target_os = "windows")]
    capture_thread_mmcss: Arc<AtomicU8>,
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

        #[cfg(target_os = "windows")]
        let capture_thread_mmcss = Arc::new(AtomicU8::new(0));
        #[cfg(target_os = "windows")]
        let capture_thread_mmcss_cb = Arc::clone(&capture_thread_mmcss);
        #[cfg(target_os = "windows")]
        let mut capture_thread_mmcss_attempted = false;
        let metrics = Arc::new(Metrics::new());
        let metrics_cb = Arc::clone(&metrics);
        let metrics_err = Arc::clone(&metrics);
        let audio_arrival = Arc::new(AudioArrival::default());
        let audio_arrival_cb = Arc::clone(&audio_arrival);

        let producer_gate = Arc::new(ProducerGate::default());
        let producer_gate_cb = Arc::clone(&producer_gate);
        // Fixed callback metadata storage; no callback allocation.
        let (mut stamp_producer, stamp_consumer) = HeapRb::<CallbackStamp>::new(4096).split();

        let in_chans = input_channels as usize;
        let mut downmix = MonoDownmix::new(in_chans, input_rate);

        let err_fn = move |e: cpal::Error| {
            metrics_err.record_async_error(CaptureErrorKind::from_cpal(e.kind()));
        };

        macro_rules! build_pcm_stream {
            ($sample:ty) => {
                device.build_input_stream(
                    stream_config,
                    move |data: &[$sample], info| {
                        let Some(admission) = producer_gate_cb.enter() else {
                            return;
                        };
                        #[cfg(target_os = "windows")]
                        join_capture_thread_mmcss(
                            &mut capture_thread_mmcss_attempted,
                            &capture_thread_mmcss_cb,
                        );
                        let t0 = Instant::now();
                        let packet_fits = producer.vacant_len() >= data.len() / in_chans.max(1);
                        push_mono_pcm(data, in_chans, &mut downmix, &mut producer, &metrics_cb);
                        if admission.epoch != 0 {
                            let timestamp = info.timestamp();
                            if stamp_producer
                                .try_push(CallbackStamp {
                                    epoch: admission.epoch,
                                    capture: timestamp.capture,
                                    callback: timestamp.callback,
                                    frames: data.len() / in_chans.max(1),
                                })
                                .is_err()
                            {
                                producer_gate_cb
                                    .metadata_overflow
                                    .store(true, Ordering::Release);
                            } else if packet_fits {
                                producer_gate_cb
                                    .packet_committed(admission.epoch, timestamp.capture);
                            }
                        }
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
                move |data: &[f32], info| {
                    let Some(admission) = producer_gate_cb.enter() else {
                        return;
                    };
                    #[cfg(target_os = "windows")]
                    join_capture_thread_mmcss(
                        &mut capture_thread_mmcss_attempted,
                        &capture_thread_mmcss_cb,
                    );
                    let t0 = Instant::now();
                    let packet_fits = producer.vacant_len() >= data.len() / in_chans.max(1);
                    push_mono_f32(data, in_chans, &mut downmix, &mut producer, &metrics_cb);
                    if admission.epoch != 0 {
                        let timestamp = info.timestamp();
                        if stamp_producer
                            .try_push(CallbackStamp {
                                epoch: admission.epoch,
                                capture: timestamp.capture,
                                callback: timestamp.callback,
                                frames: data.len() / in_chans.max(1),
                            })
                            .is_err()
                        {
                            producer_gate_cb
                                .metadata_overflow
                                .store(true, Ordering::Release);
                        } else if packet_fits {
                            producer_gate_cb.packet_committed(admission.epoch, timestamp.capture);
                        }
                    }
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
            _stream: Arc::new(stream),
            producer_gate,
            stamp_consumer,
            precise: None,
            consumer,
            resampler,
            metrics,
            #[cfg(target_os = "windows")]
            capture_thread_mmcss,
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

    /// Close old producer admission, wait for its current callback, discard old
    /// data, then open a new precise epoch. Call before resume, or on a warm stream.
    pub fn begin_precise_epoch(&mut self, timeout: Duration) -> Result<CaptureCutHandle> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("precise capture deadline overflow"))?;
        self.producer_gate
            .check_epoch(self.producer_gate.current_epoch())?;
        self.producer_gate
            .close(self.producer_gate.current_epoch())?;
        if let Err(error) = self.producer_gate.wait_closed_until(deadline) {
            self.producer_gate
                .timeout(self.producer_gate.current_epoch());
            return Err(error);
        }
        self.precise = None;
        self.flush();
        while self.stamp_consumer.try_pop().is_some() {}
        self.ensure_stream_healthy()?;
        let before = self.metrics.snapshot();
        let start = self._stream.now();
        if Instant::now() >= deadline {
            self.producer_gate
                .timeout(self.producer_gate.current_epoch());
            return Err(anyhow!("precise capture begin deadline exhausted"));
        }
        let epoch = self.producer_gate.open_next()?;
        self.precise = Some(PreciseEpoch {
            epoch,
            start,
            native: Vec::new(),
            stamps: Vec::new(),
            overflow_before: before.input_overflow_count,
            errors_before: before.async_error_count,
            cached: None,
        });
        Ok(CaptureCutHandle {
            stream: Arc::downgrade(&self._stream),
            gate: Arc::clone(&self.producer_gate),
            epoch,
            start,
            cut: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// After synchronous cut_now, keep draining FIFO until a complete packet
    /// starts at/after cut. Quiesce, pause, then render retained native [start,cut)
    /// from fresh resampler state. Prior live reads remain part of full clip.
    /// No watermark or failure is never returned as successful audio.
    pub fn seal_precise(&mut self, cut: &CaptureCutToken, timeout: Duration) -> Result<Vec<f32>> {
        let precise = self
            .precise
            .as_ref()
            .ok_or_else(|| anyhow!("no precise capture epoch"))?;
        if !Arc::ptr_eq(&cut.gate, &self.producer_gate) || precise.epoch != cut.epoch {
            return Err(anyhow!("precise capture wrong epoch or stream"));
        }
        if let Some(cached) = &precise.cached {
            return cached.clone();
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("precise capture deadline overflow"))?;
        let result = (|| {
            loop {
                self.drain_native_into_scratch();
                self.ensure_stream_healthy()?;
                let metrics = self.metrics.snapshot();
                self.precise
                    .as_ref()
                    .expect("epoch retained during seal")
                    .validate_integrity(
                        self.producer_gate.metadata_overflow.load(Ordering::Acquire),
                        metrics.input_overflow_count,
                        metrics.async_error_count,
                    )?;
                if self.producer_gate.has_watermark() {
                    break;
                }
                if Instant::now() >= deadline {
                    self.producer_gate.timeout(cut.epoch);
                    return Err(anyhow!("precise capture timeout: no post-cut watermark"));
                }
                // Drain while waiting: a delayed worker must not fill native ring.
                self.audio_arrival.wait(
                    &mut self.arrival_seen,
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(2)),
                );
            }
            if let Err(error) = self.producer_gate.wait_closed_until(deadline) {
                self.producer_gate.timeout(cut.epoch);
                return Err(error);
            }
            self.drain_native_into_scratch();
            if Instant::now() >= deadline {
                self.producer_gate.timeout(cut.epoch);
                return Err(anyhow!("precise capture quiesce deadline exhausted"));
            }
            // Synchronization is admission+watermark+quiescence. Backend pause
            // may only queue a command (WASAPI), so it is never our barrier.
            self._stream
                .pause()
                .map_err(|e| anyhow!("precise capture pause failed: {e}"))?;
            if Instant::now() >= deadline {
                self.producer_gate.timeout(cut.epoch);
                return Err(anyhow!("precise capture pause deadline exhausted"));
            }
            self.ensure_stream_healthy()?;
            let precise = self.precise.as_ref().expect("epoch retained during seal");
            let metrics = self.metrics.snapshot();
            precise.validate_integrity(
                self.producer_gate.metadata_overflow.load(Ordering::Acquire),
                metrics.input_overflow_count,
                metrics.async_error_count,
            )?;
            let selected = precise.selected_native(cut, self.input_rate)?;
            StreamResampler::resample_sealed(&selected, self.input_rate, self.target_rate)
        })();
        if result.is_err() {
            // On error stop further admission. Retain ring/native storage even
            // if callback cannot quiesce before shared deadline; never recycle it.
            let _ = self.producer_gate.close(cut.epoch);
            if Instant::now() >= deadline || self.producer_gate.wait_closed_until(deadline).is_err()
            {
                self.producer_gate.timeout(cut.epoch);
            } else {
                let _ = self._stream.pause();
            }
        }
        self.precise
            .as_mut()
            .expect("epoch retained during seal")
            .cached = Some(result.clone());
        result
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
    fn drain_native_into_scratch(&mut self) {
        self.native_scratch.clear();
        let mut tmp = [0f32; 4096];
        // Snapshot bounds each drain even when producer continues arriving.
        let mut remaining = self.consumer.occupied_len();
        while remaining > 0 {
            let take = remaining.min(tmp.len());
            let n = self.consumer.pop_slice(&mut tmp[..take]);
            if n == 0 {
                break;
            }
            self.native_scratch.extend_from_slice(&tmp[..n]);
            remaining -= n;
        }
        if let Some(precise) = self.precise.as_mut() {
            precise.native.extend_from_slice(&self.native_scratch);
            let pending_stamps = self.stamp_consumer.occupied_len();
            for _ in 0..pending_stamps {
                if let Some(stamp) = self.stamp_consumer.try_pop() {
                    precise.stamps.push(stamp);
                }
            }
        }
    }

    fn drain_native_into_scratch_and_resample(&mut self) -> Result<()> {
        self.drain_native_into_scratch();
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
        #[cfg(target_os = "windows")]
        report_capture_thread_mmcss(&self.capture_thread_mmcss);
        self.metrics.snapshot()
    }

    fn ensure_stream_healthy(&self) -> Result<()> {
        #[cfg(target_os = "windows")]
        report_capture_thread_mmcss(&self.capture_thread_mmcss);
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
        // Timed-out callback storage must remain retained until stream Drop joins it.
        if self
            .producer_gate
            .check_epoch(self.producer_gate.current_epoch())
            .is_err()
        {
            return 0;
        }
        if let Some(precise) = self.precise.as_mut() {
            precise.cached = Some(Err(anyhow!("precise capture invalidated by flush")));
        }
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
        self.producer_gate
            .check_epoch(self.producer_gate.current_epoch())?;
        self.metrics.reset_capture_epoch();
        self._stream
            .play()
            .map_err(|e| anyhow!("stream.play() on resume failed: {e}"))
    }

    pub fn stop(&mut self) -> MetricsSnapshot {
        #[cfg(target_os = "windows")]
        report_capture_thread_mmcss(&self.capture_thread_mmcss);
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // Dropping _stream on stop() would require consuming self; the
            // cpal stream will instead drop with the session struct.
            // Pause is best-effort.
            let _ = self._stream.pause();
        }
        self.metrics.snapshot()
    }
}
