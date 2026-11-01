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
        Self::parse(
            dev_var!("RIGHTKIT_CONTROL_ALIGN_ENCODER_REUSE")
                .ok()
                .as_deref(),
        )
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

#[cfg(test)]
mod tests {
    use super::skipped_locator_frames;

    #[test]
    fn encoder_reuse_switch_defaults_to_re_encoding() {
        use super::AlignEncoderReuse as R;
        assert_eq!(R::parse(None), R::Off);
        assert_eq!(R::parse(Some("0")), R::Off);
        assert_eq!(R::parse(Some("")), R::Off);
        assert_eq!(R::parse(Some("1")), R::Reuse);
        assert_eq!(R::parse(Some(" verify ")), R::Verify);
    }

    #[test]
    fn encoder_memo_hands_off_once_and_only_for_identical_audio_and_model() {
        use std::sync::Arc;
        let mut memo = super::AlignEncoderMemo::<Vec<f32>>::new();
        let audio = vec![0.25f32, -0.5, 1.0e-7];
        let encoded = Arc::new(vec![1.0f32, 2.0]);
        memo.store(7, &audio, encoded.clone());
        assert!(memo.take_matching(8, &audio).is_none(), "other model");
        assert!(memo.is_empty(), "a miss still empties the memo");

        memo.store(7, &audio, encoded.clone());
        let mut nudged = audio.clone();
        nudged[2] = f32::from_bits(nudged[2].to_bits() + 1);
        assert!(memo.take_matching(7, &nudged).is_none(), "one ulp differs");

        memo.store(7, &audio, encoded.clone());
        assert!(
            memo.take_matching(7, &audio[..2]).is_none(),
            "length differs"
        );

        memo.store(7, &audio, encoded.clone());
        let hit = memo
            .take_matching(7, &audio)
            .expect("identical audio and model");
        assert!(Arc::ptr_eq(&hit, &encoded));
        assert!(memo.take_matching(7, &audio).is_none(), "one-shot");
    }

    /// Off (default) encodes every pass; Reuse encodes once per fast → exact
    /// pair and hands back the same output; Verify encodes both, returns the
    /// fresh one and reports the comparison; a different window or model never
    /// reuses.
    #[test]
    fn align_encoder_output_reuses_only_when_switched_on() {
        use super::{
            align_encoder_output_in, AlignEncoderMemo, AlignEncoderReuse as R,
            AlignEncoderSource as Src,
        };
        use std::cell::Cell;
        use std::sync::{Arc, Mutex};
        let memo = Mutex::new(AlignEncoderMemo::new());
        let audio = vec![0.1f32; 32];
        let calls = Cell::new(0u32);
        let encode = || {
            calls.set(calls.get() + 1);
            Ok::<Vec<f32>, String>(vec![calls.get() as f32])
        };
        let same = |a: &Vec<f32>, b: &Vec<f32>| a == b;
        let pair = |mode, exact_audio: &[f32], exact_model| {
            calls.set(0);
            let (fast, _) =
                align_encoder_output_in(&memo, mode, 1, &audio, true, encode, same).unwrap();
            let (exact, src) =
                align_encoder_output_in(&memo, mode, exact_model, exact_audio, false, encode, same)
                    .unwrap();
            (fast, exact, src, calls.get())
        };

        let (fast, exact, src, n) = pair(R::Off, &audio, 1);
        assert_eq!((n, src), (2, Src::Encoded));
        assert_eq!(
            (fast.as_slice(), exact.as_slice()),
            (&[1.0][..], &[2.0][..])
        );
        assert!(memo.lock().unwrap().is_empty(), "off retains nothing");

        let (fast, exact, src, n) = pair(R::Reuse, &audio, 1);
        assert_eq!((n, src), (1, Src::Reused));
        assert!(Arc::ptr_eq(&fast, &exact));

        let (_, exact, src, n) = pair(R::Verify, &audio, 1);
        assert_eq!((n, exact.as_slice()), (2, &[2.0][..]));
        assert_eq!(src, Src::Verified { identical: false });

        let (_, _, src, n) = pair(R::Reuse, &audio[..16], 1);
        assert_eq!((n, src), (2, Src::Encoded), "different window re-encodes");
        let (_, _, src, n) = pair(R::Reuse, &audio, 2);
        assert_eq!((n, src), (2, Src::Encoded), "different model re-encodes");
        assert!(memo.lock().unwrap().is_empty());
    }

    #[test]
    fn three_frame_jump_probes_command_onset_without_repeating_visited_frame() {
        assert_eq!(
            skipped_locator_frames(114, 117, 107, 125).collect::<Vec<_>>(),
            [115, 116]
        );
    }

    #[test]
    fn probes_obey_search_window_and_leave_zero_duration_emissions_alone() {
        assert_eq!(
            skipped_locator_frames(104, 108, 107, 125).collect::<Vec<_>>(),
            [107]
        );
        assert_eq!(
            skipped_locator_frames(123, 127, 107, 125).collect::<Vec<_>>(),
            [124]
        );
        assert!(skipped_locator_frames(110, 110, 107, 125).is_empty());
        assert!(skipped_locator_frames(110, 111, 107, 125).is_empty());
        assert!(skipped_locator_frames(130, 134, 107, 125).is_empty());
    }
}
