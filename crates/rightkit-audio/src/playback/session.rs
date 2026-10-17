//! cpal output stream, device selection, and device-loss recovery.

use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, StreamConfig, SupportedStreamConfig};

use super::render::fill_block;
use super::state::{OutputStats, Shared, FAULT_REOPEN};
use crate::error::AudioError;

/// Ring length used by [`OutputSession::open`].
pub const DEFAULT_RING_DURATION: Duration = Duration::from_secs(2);
const MAX_RING_DURATION: Duration = Duration::from_secs(60);
/// Samples rendered per pass inside the callback. The scratch buffer is allocated once, before
/// the stream starts; a larger device buffer is processed in several passes.
const SCRATCH_SAMPLES: usize = 4096;
/// Minimum wait between failed reopen attempts.
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

fn playback_err(context: &str, e: impl std::fmt::Display) -> AudioError {
    AudioError::Playback(format!("{context}: {e}"))
}

/// One output device as the host reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    /// Display name. Pass it to [`OutputSession::open`] to select the device.
    pub name: String,
    /// Stable host identifier when the host provides one. [`OutputSession::open`] also accepts
    /// it, which disambiguates devices that share a name.
    pub platform_id: Option<String>,
    /// Default configuration rate, 0 when the device did not report one.
    pub sample_rate: u32,
    /// Default configuration channel count, 0 when the device did not report one.
    pub channels: u16,
    pub is_default: bool,
}

/// The format an open session expects: interleaved `f32` at this rate and channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Ring capacity in interleaved samples.
    pub capacity_samples: usize,
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unknown>".into())
}

/// Enumerate output devices. Opens nothing and retains nothing.
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_device = host.default_output_device();
    let default_id = default_device
        .as_ref()
        .and_then(|d| d.id().ok())
        .map(|id| id.id().to_string());
    let devices = host
        .output_devices()
        .map_err(|e| playback_err("output_devices() failed", e))?;
    let mut out = Vec::new();
    for (idx, device) in devices.enumerate() {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| format!("device-{idx}"));
        let platform_id = device.id().ok().map(|id| id.id().to_string());
        let (sample_rate, channels) = device
            .default_output_config()
            .map(|c| (c.sample_rate(), c.channels()))
            .unwrap_or((0, 0));
        let is_default = match (default_id.as_deref(), platform_id.as_deref()) {
            (Some(d), Some(c)) => d == c,
            _ => default_device.as_ref().is_some_and(|d| d == &device),
        };
        out.push(OutputDeviceInfo {
            name,
            platform_id,
            sample_rate,
            channels,
            is_default,
        });
    }
    Ok(out)
}

/// `None` is the default output device. `Some(s)` matches a platform id first, then a name.
fn resolve_output_device(selector: Option<&str>) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    let Some(wanted) = selector else {
        return host
            .default_output_device()
            .ok_or_else(|| AudioError::Playback("no default output device available".into()));
    };
    let devices = host
        .output_devices()
        .map_err(|e| playback_err("output_devices() failed", e))?;
    let mut by_name = None;
    for device in devices {
        if device.id().ok().is_some_and(|id| id.id() == wanted) {
            return Ok(device);
        }
        if by_name.is_none() && device_name(&device) == wanted {
            by_name = Some(device);
        }
    }
    by_name.ok_or_else(|| AudioError::Playback(format!("output device '{wanted}' not found")))
}

/// Pick the configuration that reproduces an open session's rate and channel count on a new
/// device, preferring `f32`. The ring is sized and framed for that format, so recovery cannot
/// accept a different one.
fn matching_config(
    device: &cpal::Device,
    rate: u32,
    channels: u16,
) -> Result<SupportedStreamConfig, AudioError> {
    let ranges = device
        .supported_output_configs()
        .map_err(|e| playback_err("supported_output_configs() failed", e))?;
    let mut best: Option<SupportedStreamConfig> = None;
    for range in ranges {
        if range.channels() != channels || !range.contains_rate(rate) {
            continue;
        }
        let Some(config) = range.try_with_sample_rate(rate) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some(b) => {
                config.sample_format() == SampleFormat::F32
                    && b.sample_format() != SampleFormat::F32
            }
        };
        if better {
            best = Some(config);
        }
    }
    best.ok_or_else(|| {
        AudioError::Playback(format!(
            "default output device cannot play {rate} Hz x {channels} ch; open a new session"
        ))
    })
}

/// Callback state. Owned by the audio thread; everything it shares is atomic.
struct Callback {
    shared: Arc<Shared>,
    scratch: Box<[f32]>,
    seen_flush: u64,
    gain: f32,
}

impl Callback {
    fn new(shared: Arc<Shared>) -> Self {
        let ch = shared.channels as usize;
        let block = (SCRATCH_SAMPLES / ch).max(1) * ch;
        let gain = shared.target_gain();
        let seen_flush = shared.current_flush_gen();
        Self {
            shared,
            scratch: vec![0.0f32; block].into_boxed_slice(),
            seen_flush,
            gain,
        }
    }

    /// The audio-thread body. No allocation, no locks, no blocking, no panics on any input.
    fn render<T>(&mut self, out: &mut [T], info: &cpal::OutputCallbackInfo)
    where
        T: Sample + FromSample<f32>,
    {
        let ts = info.timestamp();
        self.shared
            .set_latency(ts.playback.saturating_duration_since(ts.callback));
        self.shared.process_flush(&mut self.seen_flush);

        let ch = self.shared.channels as usize;
        let paused = self.shared.is_paused();
        let target = self.shared.target_gain();
        let usable = out.len() - out.len() % ch;
        let (body, tail) = out.split_at_mut(usable);
        let block = self.scratch.len();
        for chunk in body.chunks_mut(block) {
            let scratch = &mut self.scratch[..chunk.len()];
            let outcome = fill_block(&self.shared.ring, scratch, ch, paused, self.gain, target);
            if outcome.silent > 0 {
                self.shared.record_underrun(outcome.silent);
            }
            self.gain = target;
            for (dst, src) in chunk.iter_mut().zip(scratch.iter()) {
                *dst = T::from_sample(*src);
            }
        }
        // A device buffer that is not a whole number of frames: keep the ring frame aligned
        // and give the stray samples silence.
        for dst in tail {
            *dst = T::EQUILIBRIUM;
        }
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + FromSample<f32> + Send + 'static,
{
    let mut callback = Callback::new(Arc::clone(shared));
    let err_shared = Arc::clone(shared);
    device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| callback.render(data, info),
        move |e: cpal::Error| {
            let lost = matches!(
                e.kind(),
                cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated
            );
            err_shared.record_stream_error(lost);
        },
        None,
    )
}

fn build_and_start(
    device: &cpal::Device,
    supported: SupportedStreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, AudioError> {
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let built = match format {
        SampleFormat::F32 => build_typed::<f32>(device, config, shared),
        SampleFormat::F64 => build_typed::<f64>(device, config, shared),
        SampleFormat::I8 => build_typed::<i8>(device, config, shared),
        SampleFormat::I16 => build_typed::<i16>(device, config, shared),
        SampleFormat::I24 => build_typed::<cpal::I24>(device, config, shared),
        SampleFormat::I32 => build_typed::<i32>(device, config, shared),
        SampleFormat::I64 => build_typed::<i64>(device, config, shared),
        SampleFormat::U8 => build_typed::<u8>(device, config, shared),
        SampleFormat::U16 => build_typed::<u16>(device, config, shared),
        SampleFormat::U24 => build_typed::<cpal::U24>(device, config, shared),
        SampleFormat::U32 => build_typed::<u32>(device, config, shared),
        SampleFormat::U64 => build_typed::<u64>(device, config, shared),
        other => {
            return Err(AudioError::Playback(format!(
                "unsupported output sample format {other:?}"
            )))
        }
    };
    let stream = built.map_err(|e| playback_err("build_output_stream failed", e))?;
    stream
        .play()
        .map_err(|e| playback_err("stream.play() failed", e))?;
    Ok(stream)
}

/// Thread-safe handle to a session's atomic controls and clock. Clone it freely and hand it
/// to a UI or clock thread; the producer thread keeps the [`OutputSession`] itself.
#[derive(Clone)]
pub struct OutputControl {
    shared: Arc<Shared>,
}

impl OutputControl {
    pub fn format(&self) -> OutputFormat {
        OutputFormat {
            sample_rate: self.shared.sample_rate,
            channels: self.shared.channels,
            capacity_samples: self.shared.ring.capacity(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.shared.channels
    }

    /// Stop consuming. The device keeps running and receives silence; queued samples stay
    /// queued and the clock holds.
    pub fn pause(&self) {
        self.shared.set_paused(true);
    }

    pub fn resume(&self) {
        self.shared.set_paused(false);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.is_paused()
    }

    /// Linear gain, `0.0..=4.0` (1.0 is unity; values above 1 are clamped at full scale).
    /// Rejects NaN, infinity and out-of-range values.
    pub fn set_volume(&self, volume: f32) -> Result<(), AudioError> {
        if !volume.is_finite() || !(0.0..=4.0).contains(&volume) {
            return Err(AudioError::Invalid(format!(
                "volume must be within 0.0..=4.0, got {volume}"
            )));
        }
        self.shared.set_volume(volume);
        Ok(())
    }

    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    /// Mute without losing the volume setting. Changes are ramped inside one callback.
    pub fn set_muted(&self, muted: bool) {
        self.shared.set_muted(muted);
    }

    pub fn is_muted(&self) -> bool {
        self.shared.is_muted()
    }

    /// Drop every queued sample and reset the clock to zero (use on seek). The clock reads
    /// zero as soon as this returns; the device-side queue is dropped on the next callback.
    /// Samples pushed after this call are kept.
    pub fn flush(&self) {
        self.shared.flush();
    }

    /// Interleaved samples played since open or the last flush, latency-adjusted. Samples of
    /// silence substituted on underrun, and time spent paused, are not counted, so the clock
    /// stalls with the data rather than running ahead of it.
    pub fn played_samples(&self) -> u64 {
        self.shared.played_samples()
    }

    pub fn played_frames(&self) -> u64 {
        self.shared.played_samples() / self.shared.channels as u64
    }

    /// Playback clock for A/V sync: [`played_frames`](Self::played_frames) at the device
    /// rate. Latency is subtracted using the host's output timestamps (the gap between
    /// callback time and predicted play time) as of the latest callback; a host that reports
    /// no gap gives an unadjusted clock.
    pub fn position(&self) -> Duration {
        self.shared.position()
    }

    /// Output latency as of the latest callback.
    pub fn latency(&self) -> Duration {
        self.shared.latency()
    }

    pub fn stats(&self) -> OutputStats {
        self.shared.stats()
    }
}

/// An open output stream with a bounded lock-free ring in front of it.
///
/// Single producer: [`push_samples`](Self::push_samples) takes `&mut self`. Everything else is
/// available through `Deref` to [`OutputControl`], or from other threads via
/// [`control`](Self::control).
pub struct OutputSession {
    control: OutputControl,
    stream: Option<cpal::Stream>,
    device_name: String,
    next_retry: Instant,
    last_error: Option<AudioError>,
}

impl Deref for OutputSession {
    type Target = OutputControl;
    fn deref(&self) -> &OutputControl {
        &self.control
    }
}

impl OutputSession {
    /// Open `device` (`None` = default; otherwise a name or platform id from
    /// [`list_output_devices`]) with a [`DEFAULT_RING_DURATION`] ring. The stream starts
    /// running (silence until samples are pushed). The format is the device's default rate and
    /// channel count; read it back with [`format`](OutputControl::format).
    pub fn open(device: Option<&str>) -> Result<Self, AudioError> {
        Self::open_with_capacity(device, DEFAULT_RING_DURATION)
    }

    /// As [`open`](Self::open) with the ring sized to `capacity` of audio at the device rate
    /// (0 to 60 s, exclusive of 0).
    pub fn open_with_capacity(
        device: Option<&str>,
        capacity: Duration,
    ) -> Result<Self, AudioError> {
        if capacity.is_zero() || capacity > MAX_RING_DURATION {
            return Err(AudioError::Invalid(format!(
                "ring capacity must be within (0, {}] s, got {capacity:?}",
                MAX_RING_DURATION.as_secs()
            )));
        }
        let dev = resolve_output_device(device)?;
        let name = device_name(&dev);
        let supported = dev.default_output_config().map_err(|e| {
            playback_err(&format!("default_output_config() failed for '{name}'"), e)
        })?;
        let rate = supported.sample_rate();
        let channels = supported.channels();
        if rate == 0 || channels == 0 {
            return Err(AudioError::Playback(format!(
                "device '{name}' reported an empty output format ({rate} Hz, {channels} ch)"
            )));
        }
        let capacity_frames =
            ((capacity.as_nanos() * rate as u128).div_ceil(1_000_000_000) as usize).max(1);
        let shared = Arc::new(Shared::new(rate, channels, capacity_frames));
        let stream = build_and_start(&dev, supported, &shared)?;
        Ok(Self {
            control: OutputControl { shared },
            stream: Some(stream),
            device_name: name,
            next_retry: Instant::now(),
            last_error: None,
        })
    }

    /// Name of the device currently in use (changes after a recovery).
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Clone a handle for other threads (clock reads, pause, volume, flush, stats).
    pub fn control(&self) -> OutputControl {
        self.control.clone()
    }

    /// Queue interleaved `f32` samples in the format from [`format`](OutputControl::format).
    /// Returns how many samples were accepted, always a whole number of frames; the rest were
    /// refused (overrun) and are counted. Never blocks. If a device loss is pending this also
    /// tries to reopen the default device, at most once per 250 ms; see
    /// [`take_last_error`](Self::take_last_error).
    pub fn push_samples(&mut self, samples: &[f32]) -> usize {
        if self.control.shared.fault() == FAULT_REOPEN && Instant::now() >= self.next_retry {
            if let Err(e) = self.recover() {
                self.last_error = Some(e);
                self.next_retry = Instant::now() + RETRY_INTERVAL;
            }
        }
        self.control.shared.push(samples)
    }

    /// Samples the ring could accept right now.
    pub fn free_samples(&self) -> usize {
        self.control
            .shared
            .ring
            .capacity()
            .saturating_sub(self.control.shared.ring.len())
    }

    /// Reopen the default output device if a device loss was reported. Returns `Ok(true)` when
    /// the stream was rebuilt (queued samples, pause, volume and mute carry over), `Ok(false)`
    /// when nothing was wrong. The new device must offer the session's rate and channel count;
    /// otherwise this errors and the caller opens a new session at the new format.
    pub fn recover(&mut self) -> Result<bool, AudioError> {
        let shared = &self.control.shared;
        if shared.fault() != FAULT_REOPEN {
            return Ok(false);
        }
        // Release the dead stream first so an exclusive device can be taken again.
        self.stream = None;
        let host = cpal::default_host();
        let dev = host
            .default_output_device()
            .ok_or_else(|| AudioError::Playback("no default output device available".into()))?;
        let supported = matching_config(&dev, shared.sample_rate, shared.channels)?;
        let stream = build_and_start(&dev, supported, shared)?;
        shared.clear_fault();
        shared.note_recovery();
        self.device_name = device_name(&dev);
        self.stream = Some(stream);
        self.last_error = None;
        Ok(true)
    }

    /// The last automatic recovery failure, if any (cleared on read and on a later success).
    pub fn take_last_error(&mut self) -> Option<AudioError> {
        self.last_error.take()
    }
}

impl Drop for OutputSession {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
            drop(stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_volume_without_changing_it() {
        let control = OutputControl {
            shared: Arc::new(Shared::new(48_000, 2, 16)),
        };
        assert!(control.set_volume(f32::NAN).is_err());
        assert!(control.set_volume(-0.1).is_err());
        assert!(control.set_volume(5.0).is_err());
        assert_eq!(control.volume(), 1.0);
        control.set_volume(0.5).unwrap();
        assert_eq!(control.volume(), 0.5);
    }

    #[test]
    fn control_handle_pause_mute_flush_are_visible_to_the_callback_state() {
        let control = OutputControl {
            shared: Arc::new(Shared::new(48_000, 2, 16)),
        };
        control.pause();
        assert!(control.shared.is_paused());
        control.resume();
        assert!(!control.is_paused());
        control.set_muted(true);
        assert_eq!(control.shared.target_gain(), 0.0);
        control.set_muted(false);
        control.set_volume(0.25).unwrap();
        assert_eq!(control.shared.target_gain(), 0.25);
    }

    /// Opens the real default output device and plays 100 ms of silence. Ignored by default:
    /// hosted CI has no audio device. Run with `--features playback -- --ignored`.
    #[test]
    #[ignore]
    fn opens_default_device() {
        let mut session = OutputSession::open(None).expect("open default output device");
        let format = session.format();
        assert!(format.sample_rate > 0);
        assert!(format.channels > 0);
        let frames = format.sample_rate as usize / 10;
        let silence = vec![0.0f32; frames * format.channels as usize];
        assert_eq!(session.push_samples(&silence), silence.len());
        session.flush();
        assert_eq!(session.position(), Duration::ZERO);
    }
}
