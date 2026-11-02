//! Idle wake-word memory probe.
//!
//! Loads the shipped KWS cascade, starts the idle hey-wake stream and
//! feeds synthetic low-level noise through `feed_idle_wake` exactly as the
//! engine's wake listener does (20 ms capture blocks, contiguous sample
//! cursor), then holds so `footprint`, `heap` and `malloc_history` (macOS) or
//! `Get-Process` / VMMap (Windows) can inspect the process from outside. Run with
//! `MallocStackLogging=1` to attribute retained blocks to their allocation stacks.
//!
//! usage: kws_idle_memory <kws-model-dir> [audio-seconds=600] [hold-seconds=120]
//!          [--ort ONNXRUNTIME_LIB]
//! The spotter runs on the ONNX Runtime bound here (`--ort`, else `ORT_DYLIB_PATH`).
//! Memory is per process, so run each configuration in its own process.

use rightkit_dictation_inference::kws::Kws;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SAMPLE_RATE: usize = 16_000;
/// One capture block: 20 ms at 16 kHz.
const CHUNK: usize = SAMPLE_RATE / 50;

fn main() {
    let usage = "usage: kws_idle_memory <kws-model-dir> [audio-seconds] [hold-seconds] \
[--ort ONNXRUNTIME_LIB]";
    let mut positional = Vec::new();
    let mut ort = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ort" => ort = Some(PathBuf::from(args.next().expect("--ort needs a path"))),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let model_dir = PathBuf::from(positional.next().expect(usage));
    let audio_seconds: usize = positional
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let hold_seconds: u64 = positional
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);

    // The spotter's ONNX sessions need a runtime bound through rightkit-onnx.
    if let Some(runtime) = ort.or_else(|| std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from)) {
        rightkit_onnx::configure_runtime_with_options(
            [rightkit_onnx::explicit_file_candidate(
                std::path::absolute(&runtime).expect("absolute ONNX Runtime path"),
            )],
            &rightkit_onnx::RuntimeBindingOptions {
                force_replace: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("bind ONNX Runtime {}: {error}", runtime.display()));
    }

    let mut kws = Kws::load(&model_dir).expect("load the KWS cascade");
    kws.start_idle_wake(0.5).expect("start idle wake stream");
    println!("pid {}", std::process::id());

    let mut seed: u32 = 0x9E37_79B9;
    let mut cursor = 0usize;
    let total_samples = audio_seconds * SAMPLE_RATE;
    let started = Instant::now();
    let mut blocks = 0usize;
    while cursor < total_samples {
        let audio: Vec<f32> = (0..CHUNK)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.002
            })
            .collect();
        let end = cursor + CHUNK;
        match kws.feed_idle_wake(&audio, cursor, end) {
            Ok(None) => cursor = end,
            Ok(Some(hit)) => {
                // A hit stops the stream; the listener restarts it with a fresh
                // sample clock.
                println!("hey-wake hit at samples {hit:?}; restarting stream");
                kws.start_idle_wake(0.5).expect("restart idle wake stream");
                cursor = 0;
            }
            Err(error) => {
                eprintln!("feed_idle_wake failed at sample {cursor}: {error}");
                break;
            }
        }
        blocks += 1;
        if blocks.is_multiple_of(SAMPLE_RATE / CHUNK * 60) {
            println!(
                "fed {} s of audio in {:.1} s wall",
                cursor / SAMPLE_RATE,
                started.elapsed().as_secs_f32()
            );
        }
    }
    println!(
        "fed {} s of audio in {:.1} s wall; holding {hold_seconds} s",
        cursor / SAMPLE_RATE,
        started.elapsed().as_secs_f32()
    );
    std::thread::sleep(Duration::from_secs(hold_seconds));
}
