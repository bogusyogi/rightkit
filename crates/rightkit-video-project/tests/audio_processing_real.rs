//! Real-ffmpeg processing of the synthetic talking-head clip mixed with added noise and
//! sibilance bursts. Fails, printing `SKIP: <reason>`, when the editing ffmpeg or the clip is
//! absent.
//! Run with `-- --nocapture` to see the before/after measurements.

use std::path::{Path, PathBuf};

use rightkit_audio::{
    measure::{peak_db, rms_db},
    AudioTools, Preset,
};
use rightkit_ffmpeg::{run_checked, CancellationToken, DevFfmpeg, FfmpegCommand};
use std::time::Duration;

/// The pauses between the clip's speech spans (the cut gaps), ms.
const GAPS: [(i64, i64); 6] = [
    (2467, 3001),
    (7025, 7388),
    (10221, 10783),
    (13093, 13585),
    (16137, 16593),
    (19147, 19660),
];
const BURSTS_S: [f64; 8] = [1.0, 4.0, 5.5, 8.5, 11.5, 14.5, 17.5, 20.5];

fn skip_missing(what: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what}");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

fn tools() -> Option<DevFfmpeg> {
    match std::env::var_os("RIGHTKIT_FFMPEG").map(PathBuf::from) {
        Some(p) if p.is_file() => Some(DevFfmpeg::unverified(p)),
        _ => {
            skip_missing("no editing ffmpeg (looked at RIGHTKIT_FFMPEG)");
            None
        }
    }
}

fn clip() -> Option<PathBuf> {
    match std::env::var_os("RIGHTKIT_TALKING_HEAD_CLIP").map(PathBuf::from) {
        Some(p) if p.is_file() => Some(p),
        _ => {
            skip_missing("no synthetic talking-head clip (looked at RIGHTKIT_TALKING_HEAD_CLIP)");
            None
        }
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    noisy: PathBuf,
}

/// Clip audio + pink noise floor + sibilant bursts inside speech.
fn fixture(t: &AudioTools<'_>, clip: &Path) -> Fixture {
    {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let noisy = dir.join("noisy.wav");
        let enable: String = BURSTS_S
            .iter()
            .map(|b| format!("between(t,{b},{})", b + 0.15))
            .collect::<Vec<_>>()
            .join("+");
        let graph = format!(
            "[0:a]aformat=sample_rates=48000:channel_layouts=mono[s];\
             [1:a]aformat=sample_rates=48000:channel_layouts=mono[n];\
             [2:a]aformat=sample_rates=48000:channel_layouts=mono,highpass=f=5500,lowpass=f=9000,\
             volume='if({enable},1,0)':eval=frame[sib];\
             [s][n][sib]amix=inputs=3:normalize=0:duration=first[m]"
        );
        let mut cmd = FfmpegCommand::new();
        cmd.texts(["-hide_banner", "-loglevel", "error", "-y"])
            .input(clip)
            .texts([
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=color=pink:amplitude=0.03:sample_rate=48000:seed=7",
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=color=white:amplitude=0.6:sample_rate=48000:seed=9",
                "-filter_complex",
            ])
            .text(graph)
            .texts(["-map", "[m]", "-c:a", "pcm_f32le"])
            .path(&noisy);
        run_checked(t.ffmpeg, &cmd, t.timeout, &t.cancel).expect("build noisy fixture");
        Fixture {
            _tmp: tmp,
            dir,
            noisy,
        }
    }
}

fn gap_floor(t: &AudioTools<'_>, path: &Path) -> f64 {
    // Mean power over the middle of every gap (avoids speech tails).
    let mut p = 0.0;
    for (s, e) in GAPS {
        let (a, b) = (s + 120, e - 120);
        let x = t
            .decode_mono_f32(path, 48_000, Some((a, b - a)), None)
            .unwrap();
        p += 10f64.powf(rms_db(&x) / 10.0);
    }
    10.0 * (p / GAPS.len() as f64).log10()
}

/// Sibilance band energy relative to total energy inside the bursts, dB.
fn sibilance_ratio(t: &AudioTools<'_>, path: &Path) -> (f64, f64) {
    let (mut band, mut all) = (0.0, 0.0);
    for b in BURSTS_S {
        let w = Some(((b * 1000.0) as i64 + 20, 110));
        band += 10f64.powf(t.band_rms_db(path, 5500.0, 9000.0, w).unwrap() / 10.0);
        let x = t.decode_mono_f32(path, 48_000, w, None).unwrap();
        all += 10f64.powf(rms_db(&x) / 10.0);
    }
    let n = BURSTS_S.len() as f64;
    (10.0 * (band / n).log10(), 10.0 * (band / all).log10())
}

#[test]
fn every_filter_the_crate_emits_exists_in_the_editing_build() {
    let Some(ffmpeg) = tools() else { return };
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-filters"]);
    let out = run_checked(
        &ffmpeg,
        &cmd,
        Duration::from_secs(600),
        &CancellationToken::new(),
    )
    .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &out.stderr;
    let have: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    for f in [
        "highpass",
        "lowpass",
        "afftdn",
        "anlmdn",
        "arnndn",
        "acompressor",
        "deesser",
        "alimiter",
        "sidechaincompress",
        "volume",
        "amix",
        "aloop",
        "areverse",
        "concat",
        "adelay",
        "afade",
        "atrim",
        "asetpts",
        "apad",
        "aformat",
        "asplit",
        "ebur128",
        "astats",
        "anull",
    ] {
        assert!(
            have.contains(&f),
            "filter {f} missing from the editing ffmpeg"
        );
    }
}

#[test]
fn chain_cuts_noise_floor_and_sibilance_and_holds_the_ceiling() {
    let (Some(ffmpeg), Some(clip)) = (tools(), clip()) else {
        return;
    };
    let t = AudioTools::new(&ffmpeg);
    let fx = fixture(&t, &clip);
    let preset = Preset::builtin("talking-head").unwrap();
    let out = fx.dir.join("processed.wav");
    t.apply_chain(&fx.noisy, &out, &preset.chain).unwrap();

    let (floor_b, floor_a) = (gap_floor(&t, &fx.noisy), gap_floor(&t, &out));
    let ((sib_b, ratio_b), (sib_a, ratio_a)) =
        (sibilance_ratio(&t, &fx.noisy), sibilance_ratio(&t, &out));
    let astats_b = t.astats(&fx.noisy, Some((2587, 300)), None).unwrap();
    let astats_a = t.astats(&out, Some((2587, 300)), None).unwrap();
    let samples = t.decode_mono_f32(&out, 48_000, None, None).unwrap();
    let peak = peak_db(&samples);
    let lufs = t.measure_loudness(&out).unwrap();
    println!(
        "noise floor (gap RMS dBFS): before {floor_b:.1} after {floor_a:.1} (reduction {:.1} dB)",
        floor_b - floor_a
    );
    println!(
        "astats gap window: before rms {:.1} floor {:?}; after rms {:.1} floor {:?}",
        astats_b.rms_db, astats_b.noise_floor_db, astats_a.rms_db, astats_a.noise_floor_db
    );
    println!("sibilance band 5.5-9k in bursts: before {sib_b:.1} dBFS ({ratio_b:.1} dB rel. total) after {sib_a:.1} dBFS ({ratio_a:.1} dB rel. total)");
    println!(
        "sample peak after {peak:.2} dBFS (ceiling {:.1}); loudness {:?}",
        preset.chain.limiter.ceiling_db, lufs
    );

    assert!(
        floor_b - floor_a >= 8.0,
        "noise floor only dropped {:.1} dB",
        floor_b - floor_a
    );
    assert!(
        sib_b - sib_a >= 3.0,
        "sibilance band only dropped {:.1} dB",
        sib_b - sib_a
    );
    assert!(peak <= preset.chain.limiter.ceiling_db + 0.1, "peak {peak}");
    assert_eq!(lufs.clipped_samples, 0);
}
