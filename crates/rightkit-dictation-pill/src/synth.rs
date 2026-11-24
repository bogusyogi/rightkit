//! A short synthesized swell as a 16-bit mono PCM WAV image, for the intro sound a host hands to
//! `IntroSound::Wav`: a sine that glides between two pitches under a three-point gain envelope.
//! Every number is a [`SwellSpec`] field; the crate holds no sound of its own.

/// The swell: a sine whose frequency glides geometrically from `start_hz` to `end_hz` over
/// `glide_secs`, with gain rising geometrically from `start_gain` to `peak_gain` over
/// `attack_secs` and then falling geometrically to `end_gain` over `decay_secs`.
#[derive(Clone, Copy, Debug)]
pub struct SwellSpec {
    pub sample_rate: u32,
    /// Length of the whole sound (at most [`MAX_SECONDS`]).
    pub seconds: f64,
    pub start_hz: f64,
    pub end_hz: f64,
    pub glide_secs: f64,
    pub attack_secs: f64,
    pub decay_secs: f64,
    pub start_gain: f64,
    pub peak_gain: f64,
    pub end_gain: f64,
}

/// The longest sound [`swell_wav`] will make.
pub const MAX_SECONDS: f64 = 10.0;
/// The accepted sample-rate range, in Hz.
pub const SAMPLE_RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;

/// Why [`swell_wav`] refused a spec. Nothing was allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwellError {
    /// `sample_rate` is outside [`SAMPLE_RATES`].
    SampleRate,
    /// `seconds` is not finite, not positive, or over [`MAX_SECONDS`].
    Duration,
    /// A pitch is not finite, not positive, or above half the sample rate.
    Pitch,
    /// `glide_secs`, `attack_secs` or `decay_secs` is not finite, not positive, or over
    /// [`MAX_SECONDS`].
    Envelope,
    /// A gain is not finite or not in `(0, 1]`.
    Gain,
    /// The frame count or a WAV size does not fit its field.
    Size,
}

impl SwellError {
    /// A stable tag for diagnostics.
    pub fn code(self) -> &'static str {
        match self {
            SwellError::SampleRate => "sample_rate",
            SwellError::Duration => "duration",
            SwellError::Pitch => "pitch",
            SwellError::Envelope => "envelope",
            SwellError::Gain => "gain",
            SwellError::Size => "size",
        }
    }
}

fn secs_ok(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= MAX_SECONDS
}

fn gain_ok(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 1.0
}

/// The WAV image of `spec`: 16-bit mono at `spec.sample_rate`. Every value is validated, and the
/// frame count and the RIFF and data sizes are computed with checked arithmetic, before anything
/// is allocated; a spec outside the bounds returns an error.
pub fn swell_wav(spec: &SwellSpec) -> Result<Vec<u8>, SwellError> {
    let sample_rate = spec.sample_rate;
    if !SAMPLE_RATES.contains(&sample_rate) {
        return Err(SwellError::SampleRate);
    }
    if !secs_ok(spec.seconds) {
        return Err(SwellError::Duration);
    }
    let nyquist = f64::from(sample_rate) / 2.0;
    for hz in [spec.start_hz, spec.end_hz] {
        if !(hz.is_finite() && hz > 0.0 && hz <= nyquist) {
            return Err(SwellError::Pitch);
        }
    }
    if ![spec.glide_secs, spec.attack_secs, spec.decay_secs]
        .into_iter()
        .all(secs_ok)
    {
        return Err(SwellError::Envelope);
    }
    if ![spec.start_gain, spec.peak_gain, spec.end_gain]
        .into_iter()
        .all(gain_ok)
    {
        return Err(SwellError::Gain);
    }
    // At most 10 s at 192 kHz: 1.92 million frames, far inside every size below.
    let frames_f = (f64::from(sample_rate) * spec.seconds).floor();
    if !(frames_f.is_finite()
        && frames_f >= 1.0
        && frames_f <= f64::from(*SAMPLE_RATES.end()) * MAX_SECONDS)
    {
        return Err(SwellError::Size);
    }
    let frames = frames_f as usize;
    let data_bytes = frames.checked_mul(2).ok_or(SwellError::Size)?;
    let payload = u32::try_from(data_bytes).map_err(|_| SwellError::Size)?;
    let riff = payload.checked_add(36).ok_or(SwellError::Size)?;
    let byte_rate = sample_rate.checked_mul(2).ok_or(SwellError::Size)?;
    let total = data_bytes.checked_add(44).ok_or(SwellError::Size)?;
    let mut out: Vec<u8> = Vec::with_capacity(total);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&payload.to_le_bytes());
    // Interpolate in log space. Ratios of valid positive inputs can overflow near zero.
    let [start_pitch, end_pitch] = [spec.start_hz.ln(), spec.end_hz.ln()];
    let [start_gain, peak_gain, end_gain] = [spec.start_gain.ln(), spec.peak_gain.ln(), spec.end_gain.ln()];
    let interpolate = |start: f64, end: f64, progress: f64| {
        (start + (end - start) * progress).exp()
    };
    let mut phase = 0.0f64;
    for frame in 0..frames {
        let time = frame as f64 / f64::from(sample_rate);
        let progress = (time / spec.glide_secs).min(1.0);
        let frequency = interpolate(start_pitch, end_pitch, progress);
        let gain = if time <= spec.attack_secs {
            interpolate(start_gain, peak_gain, time / spec.attack_secs)
        } else {
            let progress = ((time - spec.attack_secs) / spec.decay_secs).min(1.0);
            interpolate(peak_gain, end_gain, progress)
        };
        let sample = ((phase.sin() * gain).clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
        phase += std::f64::consts::TAU * frequency / f64::from(sample_rate);
    }
    Ok(out)
}
