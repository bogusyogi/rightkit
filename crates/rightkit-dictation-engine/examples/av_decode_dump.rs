//! Measurement only — decode files through the AVFoundation path and write the
//! resulting 16 kHz mono PCM as WAV, so the same corpus can be transcribed once
//! per decoder and the WER compared. `av_decode::decode_to_16k_mono` is the pure
//! AVFoundation path (file_transcribe's wrapper falls back to ffmpeg, which would
//! defeat the comparison).
//!
//! Usage: av_decode_dump <out_dir> <file> [<file> ...]

#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(target_os = "macos")]
use rightkit_dictation_engine::av_decode;

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: av_decode_dump <out_dir> <file> [<file> ...]");
        std::process::exit(2);
    }
    let out_dir = Path::new(&args[0]);
    std::fs::create_dir_all(out_dir).expect("create out dir");

    for input in &args[1..] {
        let path = Path::new(input);
        match av_decode::decode_to_16k_mono(path) {
            Ok(samples) => {
                let name = path.file_stem().unwrap().to_string_lossy().to_string();
                let out = out_dir.join(format!("{name}.wav"));
                rightkit_audio::wav::write_wav_mono_i16(&out, 16_000, &samples).expect("write wav");
                println!("av  {name}: {} samples", samples.len());
            }
            Err(e) => eprintln!("av  {input}: DECLINED ({e})"),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
