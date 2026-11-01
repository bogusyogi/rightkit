//! macOS-native media decode via AVFoundation, replacing the ffmpeg subprocess.
//!
//! Measured 2026-07-18 on macOS 26.5 against the ffmpeg path this fronts:
//! 145 ms vs 2 860 ms for a 37.6 s file. Almost all of ffmpeg's time was process
//! spawn + dylib load (its own CPU time was 0.09 s), so what this removes is a
//! fixed ~2.7 s tax paid on every job regardless of file length, plus a
//! subprocess boundary and its error handling.
//!
//! Output fidelity, verified in-process by `examples/av_decode_check.rs`:
//!   * lossless (WAV/FLAC): **byte-identical** to ffmpeg (max|Δ| 0).
//!   * lossy (mp3/aac): the same audio offset by the codec's priming delay
//!     (mp3 −576 samples / 36 ms), aligned max|Δ| ~3e-5 — inaudible and
//!     irrelevant to ASR. NOT sample-index-identical, by nature of lossy
//!     decoding, and that is expected, not a defect.
//!
//! **This does not remove ffmpeg.** AVFoundation cannot open webm or mkv (nor
//! wma/wv/ape), and those work today — downloaded lectures and Zoom/OBS captures
//! land in exactly those containers. `file_transcribe` falls back to ffmpeg when
//! this returns an error, so the fast path covers everything common while
//! coverage does not regress. Verified support:
//!   OK          mp3 m4a wav flac mp4 mov aiff caf aac opus ogg amr
//!   UNSUPPORTED webm mkv wma wv ape
//! Re-check with `scripts/coreml/av_format_coverage.swift`.

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVMediaTypeAudio, AVURLAsset,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

/// Decode any AVFoundation-readable file to 16 kHz mono f32 samples.
///
/// The 16 kHz/mono/s16le conversion is requested from AVFoundation directly
/// rather than resampled afterwards, so there is no second conversion stage that
/// could disagree with what ffmpeg produced.
///
/// Returns `Err` for containers AVFoundation cannot read; the caller is expected
/// to fall back to ffmpeg rather than surface this to the user.
// AVFoundation's replacement is callback-only. This decoder stays synchronous
// because it runs on a background import worker.
#[allow(deprecated)]
pub fn decode_to_16k_mono(input: &Path) -> Result<Vec<f32>, String> {
    let path = input.to_str().ok_or("path is not valid UTF-8")?;

    unsafe {
        let ns_path = NSString::from_str(path);
        let url = NSURL::fileURLWithPath(&ns_path);
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);

        // AVMediaTypeAudio is a framework global exposed as Option; it is always
        // present at runtime, so a missing value means AVFoundation itself failed
        // to load — treat that as unsupported and let ffmpeg try.
        let media_audio = AVMediaTypeAudio.ok_or("AVMediaTypeAudio unavailable")?;
        let tracks = asset.tracksWithMediaType(media_audio);
        // No audio track is the signature of a container AVFoundation does not
        // understand (webm/mkv) as much as of a genuinely silent file. Either way
        // ffmpeg is the right next attempt.
        let track = tracks
            .firstObject()
            .ok_or("no readable audio track (container may be webm/mkv)")?;

        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .map_err(|e| format!("cannot create reader: {e}"))?;

        let settings = audio_settings_16k_mono();
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &track,
            Some(&settings),
        );

        if !reader.canAddOutput(&output) {
            return Err("reader rejected 16 kHz mono PCM output".into());
        }
        reader.addOutput(&output);

        if !reader.startReading() {
            return Err(match reader.error() {
                Some(e) => format!("startReading failed: {e}"),
                None => "startReading failed".into(),
            });
        }

        let mut samples: Vec<f32> = Vec::new();
        while let Some(buffer) = output.copyNextSampleBuffer() {
            let Some(block) = buffer.data_buffer() else {
                continue;
            };
            let mut length: usize = 0;
            let mut ptr: *mut std::ffi::c_char = std::ptr::null_mut();
            let status = block.data_pointer(0, std::ptr::null_mut(), &mut length, &mut ptr);
            if status != 0 || ptr.is_null() || length < 2 {
                continue;
            }
            // Interleaved signed 16-bit LE, mono — two bytes per sample.
            let pcm = std::slice::from_raw_parts(ptr as *const u8, length);
            samples.reserve(length / 2);
            for chunk in pcm.as_chunks::<2>().0 {
                let v = i16::from_le_bytes([chunk[0], chunk[1]]);
                samples.push(v as f32 / i16::MAX as f32);
            }
        }

        if reader.status() == AVAssetReaderStatus::Failed {
            return Err(match reader.error() {
                Some(e) => format!("decode failed: {e}"),
                None => "decode failed".into(),
            });
        }
        if samples.is_empty() {
            return Err("decoded zero samples".into());
        }
        Ok(samples)
    }
}

/// Media duration without decoding. AVFoundation reads this from the container
/// header, replacing an entire `ffmpeg -i` process spawn (and its stderr parsing)
/// with a struct field read.
pub fn probe_duration_secs(input: &Path) -> Option<u32> {
    let path = input.to_str()?;
    unsafe {
        let ns_path = NSString::from_str(path);
        let url = NSURL::fileURLWithPath(&ns_path);
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let duration = asset.duration();
        if duration.timescale == 0 {
            return None;
        }
        let secs = duration.value as f64 / duration.timescale as f64;
        if !secs.is_finite() || secs <= 0.0 {
            return None;
        }
        Some(secs as u32)
    }
}

/// PCM output settings: 16 kHz, mono, signed 16-bit little-endian, interleaved —
/// the same shape `ffmpeg -ac 1 -ar 16000 -c:a pcm_s16le` emits.
unsafe fn audio_settings_16k_mono() -> Retained<NSDictionary<NSString, AnyObject>> {
    // kAudioFormatLinearPCM, i.e. the FourCC 'lpcm'.
    const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6C70_636D;

    let keys: [&NSString; 7] = [
        &NSString::from_str("AVFormatIDKey"),
        &NSString::from_str("AVSampleRateKey"),
        &NSString::from_str("AVNumberOfChannelsKey"),
        &NSString::from_str("AVLinearPCMBitDepthKey"),
        &NSString::from_str("AVLinearPCMIsFloatKey"),
        &NSString::from_str("AVLinearPCMIsBigEndianKey"),
        &NSString::from_str("AVLinearPCMIsNonInterleaved"),
    ];
    let values: [Retained<NSNumber>; 7] = [
        NSNumber::new_u32(K_AUDIO_FORMAT_LINEAR_PCM),
        NSNumber::new_f64(16000.0),
        NSNumber::new_u32(1),
        NSNumber::new_u32(16),
        NSNumber::new_bool(false),
        NSNumber::new_bool(false),
        NSNumber::new_bool(false),
    ];
    let value_refs: Vec<&AnyObject> = values
        .iter()
        .map(|v| &*(Retained::as_ptr(v) as *const AnyObject))
        .collect();

    NSDictionary::from_slices(&keys, &value_refs)
}
