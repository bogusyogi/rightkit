"""Compare Rust adaptive_hot_v2_5 against the canonical Python policy.

The Python reference here is a small, direct port of the locked policy recovered
from git commit 71186d9:

    app/heardright_v2/audio_intelligence/adaptive.py::policy_adaptive_hot_v2_5
    app/heardright_v2/audio_intelligence/condition.py::_hpf/_soft_limiter

It intentionally tests policy mechanics, not WER:
- loud/mastered audio skips HPF and boost,
- quiet high-SNR speech gets HPF + gain + limiter,
- quiet low-SNR audio gets HPF but no boost,
- near-silence remains finite and unboosted.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import webrtcvad
from scipy.signal import butter, resample_poly, sosfilt


ROOT = Path(__file__).resolve().parents[1]
CORE = ROOT / "heardright_core"
SR = 16_000

HPF_FC = 80.0
HPF_ORDER = 2
HPF_ACTIVE_RMS_GATE_DBFS = -22.0
ACTIVE_RMS_GATE_DBFS = -31.0
GAIN_TARGET_DBFS_V2 = -20.0
SNR_MIN_FOR_BOOST_DB = 12.0
LIMITER_THRESHOLD_DBFS = -1.0
VAD_AGGRESSIVENESS = 2
VAD_FRAME_MS = 30


@dataclass
class Decision:
    pre_active_rms_dbfs: float
    pre_snr_proxy_db: float
    hpf_applied: bool
    boosted: bool
    gain_db: float


def rms_dbfs(audio: np.ndarray) -> float:
    if audio.size == 0:
        return -120.0
    rms = float(np.sqrt(np.mean(audio.astype("float64") ** 2)))
    return 20.0 * np.log10(rms) if rms > 1e-9 else -120.0


def active_speech_rms_dbfs(audio: np.ndarray, sr: int = SR) -> float:
    if audio.size == 0:
        return -120.0
    vad = webrtcvad.Vad(VAD_AGGRESSIVENESS)
    frame_len = int(sr * VAD_FRAME_MS / 1000)
    if audio.size < frame_len:
        return rms_dbfs(audio)
    pcm = np.clip(audio, -1.0, 1.0)
    pcm = (pcm * 32767.0).astype(np.int16)
    chunks = []
    for start in range(0, audio.size - frame_len + 1, frame_len):
        seg = pcm[start : start + frame_len].tobytes()
        try:
            if vad.is_speech(seg, sr):
                chunks.append(audio[start : start + frame_len])
        except Exception:
            pass
    if not chunks:
        return rms_dbfs(audio)
    return rms_dbfs(np.concatenate(chunks).astype("float32"))


def snr_proxy_dbfs(audio: np.ndarray, sr: int = SR) -> float:
    if audio.size == 0:
        return 0.0
    frame_len = int(sr * 50 / 1000)
    n_full = (audio.size // frame_len) * frame_len
    if n_full == 0:
        return 0.0
    frame_rms = [
        float(np.sqrt(np.mean(frame.astype("float64") ** 2))) + 1e-9
        for frame in audio[:n_full].reshape(-1, frame_len)
    ]
    frame_rms = np.array(sorted(frame_rms), dtype=np.float64)
    top_start = min(int(0.9 * len(frame_rms)), len(frame_rms) - 1)
    bottom_count = max(int(0.1 * len(frame_rms)), 1)
    top = float(np.mean(frame_rms[top_start:]))
    bottom = float(np.mean(frame_rms[:bottom_count]))
    return 20.0 * np.log10(top / bottom) if bottom > 1e-9 else 0.0


def hpf(audio: np.ndarray, sr: int = SR) -> np.ndarray:
    sos = butter(HPF_ORDER, HPF_FC, btype="highpass", fs=sr, output="sos")
    return sosfilt(sos, audio).astype("float32")


def soft_limiter(audio: np.ndarray) -> np.ndarray:
    threshold = 10 ** (LIMITER_THRESHOLD_DBFS / 20.0)
    knee = threshold * 0.9
    out = audio.astype("float32").copy()
    above = np.abs(out) > knee
    if np.any(above):
        sign = np.sign(out[above])
        excess = np.abs(out[above]) - knee
        out[above] = sign * (
            knee + (threshold - knee) * np.tanh(excess / max(threshold - knee, 1e-9))
        )
    return out


def python_policy(audio: np.ndarray) -> tuple[np.ndarray, Decision]:
    audio = np.asarray(audio, dtype="float32").reshape(-1)
    audio = np.where(np.isfinite(audio), audio, 0.0).astype("float32")
    pre_active = active_speech_rms_dbfs(audio)
    pre_snr = snr_proxy_dbfs(audio)
    out = audio.copy()

    hpf_applied = pre_active < HPF_ACTIVE_RMS_GATE_DBFS
    if hpf_applied:
        out = hpf(out)

    boosted = False
    gain_db = 0.0
    if pre_active <= ACTIVE_RMS_GATE_DBFS and pre_snr >= SNR_MIN_FOR_BOOST_DB:
        anchor = active_speech_rms_dbfs(out)
        if anchor > -118.0:
            gain_db = max(0.0, float(GAIN_TARGET_DBFS_V2 - anchor))
            if gain_db > 0.05:
                out = (out * (10 ** (gain_db / 20.0))).astype("float32")
                out = soft_limiter(out)
                boosted = True

    return out.astype("float32"), Decision(pre_active, pre_snr, hpf_applied, boosted, gain_db)


def rust_policy(audio: np.ndarray) -> np.ndarray:
    subprocess.run(
        ["cargo", "build", "--quiet", "--bin", "condition_audio"],
        cwd=CORE,
        check=True,
    )
    exe = CORE / "target" / "debug" / ("condition_audio.exe" if sys.platform == "win32" else "condition_audio")
    proc = subprocess.run(
        [str(exe), str(SR), "adaptive_hot_v2_5"],
        input=np.asarray(audio, dtype="<f4").tobytes(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return np.frombuffer(proc.stdout, dtype="<f4").copy()


def fixtures() -> dict[str, np.ndarray]:
    t = np.arange(SR, dtype=np.float32) / SR
    quiet_gap = (0.00001 * np.sin(2 * np.pi * 73 * t)).astype("float32")
    quiet_gap[4_000:8_000] = 0.003 * np.sin(2 * np.pi * 220 * t[4_000:8_000])
    return {
        "loud_mastered": (0.18 * np.sin(2 * np.pi * 440 * t)).astype("float32"),
        "quiet_high_snr": quiet_gap,
        "quiet_low_snr": (0.003 * np.sin(2 * np.pi * 220 * t)).astype("float32"),
        "silence": np.zeros(SR, dtype="float32"),
        "nonfinite": np.array([0.0, np.nan, np.inf, -np.inf] * 500, dtype="float32"),
    }


def load_audio_file(path: Path) -> np.ndarray:
    import soundfile as sf

    audio, sr = sf.read(path, dtype="float32", always_2d=True)
    mono = audio.mean(axis=1)
    if sr != SR:
        gcd = np.gcd(sr, SR)
        mono = resample_poly(mono, SR // gcd, sr // gcd).astype("float32")
    return mono.astype("float32")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "audio",
        nargs="*",
        type=Path,
        help="Optional WAV/FLAC/etc files to compare. Uses synthetic fixtures when omitted.",
    )
    args = parser.parse_args()

    cases = fixtures()
    for path in args.audio:
        cases[f"file:{path}"] = load_audio_file(path)

    failures = []
    for name, audio in cases.items():
        py, decision = python_policy(audio)
        rs = rust_policy(audio)
        if py.shape != rs.shape:
            failures.append(f"{name}: shape mismatch {py.shape} != {rs.shape}")
            continue
        max_abs = float(np.max(np.abs(py - rs))) if py.size else 0.0
        rms_delta = abs(rms_dbfs(py) - rms_dbfs(rs))
        print(
            f"{name}: max_abs={max_abs:.8f} rms_delta_db={rms_delta:.5f} "
            f"active={decision.pre_active_rms_dbfs:.2f} snr={decision.pre_snr_proxy_db:.2f} "
            f"hpf={decision.hpf_applied} boosted={decision.boosted} gain_db={decision.gain_db:.2f}"
        )
        if max_abs > 1.0e-3 or rms_delta > 0.05:
            failures.append(f"{name}: drift max_abs={max_abs:.8f} rms_delta_db={rms_delta:.5f}")

    if failures:
        print("\nFAIL:")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print("\nPASS: Rust adaptive_hot_v2_5 matches recovered Python policy on fixtures.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
