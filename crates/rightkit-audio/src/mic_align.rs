//! Mic-to-camera alignment by normalized cross-correlation.
//!
//! Pure Rust over mono f32 samples at one shared rate; the caller decodes both inputs (for
//! example with [`crate::measure::AudioTools::decode_mono_f32`]). A coarse pass correlates
//! 4x box-decimated signals over the whole lag range; a fine pass correlates full-rate
//! samples within one coarse step of the coarse peak. The result is an integer-sample lag,
//! so its resolution is `1000 / rate` ms (0.0625 ms at 16 kHz).
//!
//! Sign convention: `camera[i]` is compared with `mic[i + lag]`. A positive `lag` means the
//! mic recording holds the same content `lag` samples later than the camera.

use crate::error::AudioError;

/// Coarse decimation factor for the full-range search.
const COARSE: usize = 4;
/// Coarse bins on each side of the peak excluded when measuring the margin.
const GUARD_COARSE: usize = 4;

/// Search parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlignParams {
    /// Largest lag searched in each direction, ms.
    pub max_lag_ms: f64,
    /// Camera reference window taken from the start of the camera signal, ms.
    pub window_ms: f64,
    /// Minimum accepted normalized correlation, in `[0, 1]`.
    pub min_ncc: f64,
}

impl Default for AlignParams {
    fn default() -> Self {
        Self {
            max_lag_ms: 1000.0,
            window_ms: 10_000.0,
            min_ncc: 0.5,
        }
    }
}

/// A confident alignment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicAlignment {
    /// Lag in samples at the analysis rate (see the module sign convention).
    pub lag_samples: i64,
    /// `lag_samples` in milliseconds: the mic content sits this far after the camera.
    pub offset_ms: f64,
    /// Normalized correlation at the chosen lag, in `[-1, 1]`; 1 is a perfect match.
    pub confidence: f64,
    /// Coarse correlation at the peak minus the best coarse correlation outside the peak's
    /// guard. A small margin means the peak is ambiguous (repeated content).
    pub margin: f64,
}

/// Estimate the lag between `camera` and `mic`.
///
/// Errors: `Invalid` for bad parameters or a zero rate; `Invalid` (message `no mic alignment`) when either signal is
/// too short or silent, or when the best correlation is below `params.min_ncc`.
pub fn align_mic(
    camera: &[f32],
    mic: &[f32],
    rate: u32,
    params: &AlignParams,
) -> Result<MicAlignment, AudioError> {
    if rate == 0 {
        return Err(AudioError::Invalid("sample rate must be positive".into()));
    }
    if !(params.max_lag_ms.is_finite() && params.max_lag_ms >= 0.0)
        || !(params.window_ms.is_finite() && params.window_ms > 0.0)
        || !(0.0..=1.0).contains(&params.min_ncc)
    {
        return Err(AudioError::Invalid(
            "alignment parameters out of range".into(),
        ));
    }
    let per_ms = rate as f64 / 1000.0;
    let max_lag = (params.max_lag_ms * per_ms).round() as usize;
    let win = ((params.window_ms * per_ms).round() as usize).min(camera.len());
    if win < 64 || mic.len() < 64 {
        return Err(AudioError::Invalid("signals are too short".into()));
    }

    let a: Vec<f64> = camera[..win].iter().map(|&x| x as f64).collect();
    let b: Vec<f64> = mic.iter().map(|&x| x as f64).collect();

    // Coarse: full lag range on decimated signals.
    let ac = decimate(&a);
    let bc = decimate(&b);
    let mc = max_lag / COARSE;
    let scores: Vec<f64> = (0..=2 * mc)
        .map(|k| ncc(&ac, &bc, k as i64 - mc as i64).unwrap_or(f64::NEG_INFINITY))
        .collect();
    let (best_k, best) =
        scores
            .iter()
            .enumerate()
            .fold((0usize, f64::NEG_INFINITY), |acc, (k, &s)| {
                if s > acc.1 {
                    (k, s)
                } else {
                    acc
                }
            });
    if !best.is_finite() {
        return Err(AudioError::Invalid(
            "camera or mic signal is silent or does not overlap".into(),
        ));
    }
    let second = scores
        .iter()
        .enumerate()
        .filter(|(k, _)| (*k as i64 - best_k as i64).unsigned_abs() as usize > GUARD_COARSE)
        .map(|(_, &s)| s)
        .fold(f64::NEG_INFINITY, f64::max);
    let margin = if second.is_finite() {
        best - second
    } else {
        best
    };

    // Fine: full-rate lags within one coarse step of the coarse peak, clamped to the requested
    // range so the result never exceeds `max_lag_ms` (zero allows only offset 0).
    let centre = (best_k as i64 - mc as i64) * COARSE as i64;
    let step = COARSE as i64;
    let limit = max_lag as i64;
    let lo = (centre - step).max(-limit);
    let hi = (centre + step).min(limit);
    let mut fine = (f64::NEG_INFINITY, centre);
    for lag in lo..=hi {
        if let Some(s) = ncc(&a, &b, lag) {
            if s > fine.0 {
                fine = (s, lag);
            }
        }
    }
    let (confidence, lag) = fine;
    if !confidence.is_finite() {
        return Err(AudioError::Invalid(
            "no full-rate lag overlaps enough signal".into(),
        ));
    }
    if confidence < params.min_ncc {
        return Err(AudioError::Invalid(format!(
            "best correlation {confidence:.3} is below {:.3}",
            params.min_ncc
        )));
    }
    Ok(MicAlignment {
        lag_samples: lag,
        offset_ms: lag as f64 * 1000.0 / rate as f64,
        confidence,
        margin,
    })
}

/// Box-average blocks of [`COARSE`] samples.
fn decimate(x: &[f64]) -> Vec<f64> {
    x.chunks(COARSE)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// Mean-removed normalized correlation of `a[i]` with `b[i + lag]` over the overlap.
/// `None` when the overlap is under half of `a` or either side has no variance.
fn ncc(a: &[f64], b: &[f64], lag: i64) -> Option<f64> {
    let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0usize, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, &x) in a.iter().enumerate() {
        let j = i as i64 + lag;
        if j < 0 || j >= b.len() as i64 {
            continue;
        }
        let y = b[j as usize];
        n += 1;
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    if n < 2 || n * 2 < a.len() {
        return None;
    }
    let nf = n as f64;
    let cov = sab - sa * sb / nf;
    let va = saa - sa * sa / nf;
    let vb = sbb - sb * sb / nf;
    if va <= f64::EPSILON * saa.max(f64::MIN_POSITIVE)
        || vb <= f64::EPSILON * sbb.max(f64::MIN_POSITIVE)
    {
        return None;
    }
    Some((cov / (va * vb).sqrt()).clamp(-1.0, 1.0))
}
