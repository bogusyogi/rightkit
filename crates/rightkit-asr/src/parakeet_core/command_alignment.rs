//! Candidate frames skipped by TDT duration jumps during control localization,
//! and the fast-pass → exact-retry encoder hand-off (perf audit ENG-08).

use std::any::Any;
use std::sync::{Arc, Mutex};

/// Probe only unvisited frames in the command search window. Callers keep the
/// decoder state reached by the original step; probes never advance that state.
pub fn skipped_locator_frames(
    previous: usize,
    next: usize,
    earliest: usize,
    latest: usize,
) -> std::ops::Range<usize> {
    let end = next.min(latest);
    previous.saturating_add(1).max(earliest).min(end)..end
}

/// `RIGHTKIT_CONTROL_ALIGN_ENCODER_REUSE` (test mode only; perf audit ENG-08). The
/// Windows exact-retry control alignment re-runs mel + a full static-window
/// encode on audio its fast pass encoded a moment earlier. Reusing that output
/// is identical only if the encoder is deterministic for identical input on the
/// device in use, which is not proven for DirectML, so the default is today's
/// re-encode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignEncoderReuse {
    /// Unset or `0`: re-encode for the exact retry (shipped behaviour).
    Off,
    /// `1`: the exact retry reuses the fast pass's encoder output.
    Reuse,
    /// `verify`: re-encode as today, compare bit-for-bit with the fast pass's
    /// output and report the verdict. Output is today's; use it to prove
    /// identity on target hardware before turning reuse on.
    Verify,
}

impl AlignEncoderReuse {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("1") | Some("on") | Some("true") => Self::Reuse,
            Some("verify") => Self::Verify,
            _ => Self::Off,
        }
    }

    pub fn from_env() -> Self {
        Self::Off
    }
}

/// One-shot hand-off of an alignment window's encoder output from the fast
/// pass to the exact retry that follows it on the same model. The retry always
/// empties the slot, and a hit needs the same model identity and bit-identical
/// conditioned audio, so nothing outlives one fast → exact pair.
#[derive(Debug)]
pub struct AlignEncoderMemo<T: ?Sized> {
    slot: Option<(usize, Vec<u32>, Arc<T>)>,
}

impl<T: ?Sized> Default for AlignEncoderMemo<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> AlignEncoderMemo<T> {
    pub const fn new() -> Self {
        Self { slot: None }
    }

    /// Remember `value` as the encoder output of `audio` on model `model_id`,
    /// replacing whatever an earlier fast pass left.
    pub fn store(&mut self, model_id: usize, audio: &[f32], value: Arc<T>) {
        self.slot = Some((model_id, audio.iter().map(|s| s.to_bits()).collect(), value));
    }

    /// Take the stored output if it was computed on this model from exactly
    /// this audio. Always leaves the memo empty.
    pub fn take_matching(&mut self, model_id: usize, audio: &[f32]) -> Option<Arc<T>> {
        let (id, bits, value) = self.slot.take()?;
        (id == model_id
            && bits.len() == audio.len()
            && bits.iter().zip(audio).all(|(b, s)| *b == s.to_bits()))
        .then_some(value)
    }

    pub fn is_empty(&self) -> bool {
        self.slot.is_none()
    }
}

type AnyEncoderOutput = dyn Any + Send + Sync;

/// The process's alignment encoder hand-off. Only the main-ASR lane aligns,
/// under its inference lease, so the lock is uncontended.
static ALIGN_ENCODER_MEMO: Mutex<AlignEncoderMemo<AnyEncoderOutput>> =
    Mutex::new(AlignEncoderMemo::new());

/// What [`align_encoder_output`] did, for the caller's log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignEncoderSource {
    /// Encoded now (switch off, fast pass, or no matching fast-pass output).
    Encoded,
    /// Reused the fast pass's output (`Reuse`).
    Reused,
    /// Encoded now and compared with the fast pass's output (`Verify`).
    Verified { identical: bool },
}

/// Encoder output for one control-alignment pass (ENG-08).
///
/// `fast_pass` is the locator pass that runs first; the exact retry follows it
/// on the same model (`model_id`) and the same conditioned `audio`. `encode`
/// is today's mel + encode. With `reuse` off this is exactly `encode()` and
/// nothing is retained. `identical` compares two outputs bit-for-bit for the
/// `Verify` mode.
pub fn align_encoder_output<T: Any + Send + Sync>(
    reuse: AlignEncoderReuse,
    model_id: usize,
    audio: &[f32],
    fast_pass: bool,
    encode: impl FnOnce() -> Result<T, String>,
    identical: impl Fn(&T, &T) -> bool,
) -> Result<(Arc<T>, AlignEncoderSource), String> {
    align_encoder_output_in(
        &ALIGN_ENCODER_MEMO,
        reuse,
        model_id,
        audio,
        fast_pass,
        encode,
        identical,
    )
}

fn align_encoder_output_in<T: Any + Send + Sync>(
    memo: &Mutex<AlignEncoderMemo<AnyEncoderOutput>>,
    reuse: AlignEncoderReuse,
    model_id: usize,
    audio: &[f32],
    fast_pass: bool,
    encode: impl FnOnce() -> Result<T, String>,
    identical: impl Fn(&T, &T) -> bool,
) -> Result<(Arc<T>, AlignEncoderSource), String> {
    if reuse == AlignEncoderReuse::Off {
        return Ok((Arc::new(encode()?), AlignEncoderSource::Encoded));
    }
    if fast_pass {
        let fresh = Arc::new(encode()?);
        if let Ok(mut memo) = memo.lock() {
            let erased: Arc<AnyEncoderOutput> = fresh.clone();
            memo.store(model_id, audio, erased);
        }
        return Ok((fresh, AlignEncoderSource::Encoded));
    }
    let previous = memo
        .lock()
        .ok()
        .and_then(|mut memo| memo.take_matching(model_id, audio))
        .and_then(|value| value.downcast::<T>().ok());
    match previous {
        Some(previous) if reuse == AlignEncoderReuse::Reuse => {
            Ok((previous, AlignEncoderSource::Reused))
        }
        previous => {
            let fresh = Arc::new(encode()?);
            let source = match previous {
                Some(previous) => AlignEncoderSource::Verified {
                    identical: identical(&previous, &fresh),
                },
                None => AlignEncoderSource::Encoded,
            };
            Ok((fresh, source))
        }
    }
}
