"""Omnilingual CTC_300M + CTC_1B  vs  RNNT/TDT baseline eval.

Downloads both sherpa-onnx int8 models, applies adaptive_hot_v2_5 conditioning,
runs on the v2_engine_gt manifest clips that exist locally, computes WER,
and prints a comparison table against the existing RNNT/TDT results.

Usage:
    py -3.11 scripts/omnilingual_eval.py
    py -3.11 scripts/omnilingual_eval.py --skip-download   # if already fetched
"""
from __future__ import annotations
import argparse, json, re, sys, time, wave
from pathlib import Path

import numpy as np
from scipy.signal import butter, sosfilt

sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path("D:/Claude/heardright")
CACHE = ROOT / "tauri-app-next/.cache/asr-eval"
MANIFEST = CACHE / "manifests/v2_engine_gt_manifest.json"
MODEL_DIR = ROOT / "model_registry/eval"
MODEL_DIR.mkdir(parents=True, exist_ok=True)

MODELS = {
    "omni_ctc_300m": {
        "hf_repo": "csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-int8-2025-11-12",
        "local":    MODEL_DIR / "omnilingual_ctc_300m_int8",
        "size":     "348 MB",
    },
    "omni_ctc_1b": {
        "hf_repo": "csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-int8-2025-11-12",
        "local":    MODEL_DIR / "omnilingual_ctc_1b_int8",
        "size":     "1.03 GB",
    },
}

# RNNT/TDT baselines from existing eval runs (adaptive_hot_v2_5, v2_engine_gt, DML)
BASELINES = {
    "rnnt_hybrid": {
        "file": "rnnt_adaptive_v2_engine_gt_dml_20260603.json",
        "overall_wer": 0.0906,
        "by_group": {"desert_social": 0.0472, "saved_8": 0.1455, "tech_vocab": 0.0896, "us_accent": 0.0000},
        "mean_decode_ms": 1053,
        "note": "DML, adaptive_hot_v2_5, bias=3",
    },
    "tdt_b64sym": {
        "file": "tdt_adaptive_v2_engine_gt_dml_20260603.json",
        "overall_wer": 0.0884,
        "by_group": {"desert_social": 0.0669, "saved_8": 0.1096, "tech_vocab": 0.1791, "us_accent": 0.0000},
        "mean_decode_ms": 2298,
        "note": "DML, adaptive_hot_v2_5, bias=3",
    },
}

SR = 16000


# ---------------------------------------------------------------------------
# Audio conditioning — adaptive_hot_v2_5  (mirrors Rust audio_conditioning.rs)
# HPF @ 80 Hz + rolling-RMS AGC → int16-quantize
# ---------------------------------------------------------------------------

def adaptive_hot_v2_5(sig_f32: np.ndarray, sr: int = SR) -> np.ndarray:
    """Apply HPF + rolling-RMS AGC, return f32 in [-1, 1] (post int16-quant rounding)."""
    # 1. 2nd-order Butterworth HPF @ 80 Hz
    sos = butter(2, 80.0 / (sr / 2.0), btype="highpass", output="sos")
    sig = sosfilt(sos, sig_f32).astype(np.float32)

    # 2. Rolling-RMS AGC
    target_rms   = 10 ** (-22.0 / 20.0)   # -22 dBFS
    max_gain     = 10 ** ( 12.0 / 20.0)   # +12 dB cap
    noise_floor  = 10 ** (-55.0 / 20.0)   # -55 dBFS

    attack_s, release_s = 0.05, 1.5
    frame = max(1, int(0.010 * sr))        # 10 ms frames
    n_frames = int(attack_s  * sr / frame) # for alpha calc
    alpha_a  = 1.0 - np.exp(-1.0 / max(1, int(attack_s  * sr / frame)))
    alpha_r  = 1.0 - np.exp(-1.0 / max(1, int(release_s * sr / frame)))

    out        = np.empty_like(sig)
    gain       = 1.0
    rms_smooth = noise_floor

    i = 0
    while i < len(sig):
        blk = sig[i:i + frame]
        rms = float(np.sqrt(np.mean(blk ** 2) + 1e-12))

        alpha   = alpha_a if rms > rms_smooth else alpha_r
        rms_smooth = rms_smooth + alpha * (rms - rms_smooth)

        if rms_smooth > noise_floor:
            desired = target_rms / (rms_smooth + 1e-12)
            gain    = min(desired, max_gain)

        out[i:i + len(blk)] = blk * gain
        i += frame

    # 3. int16-quantize (clip → round-trip)
    out_i16 = np.clip(out * 32767.0, -32768.0, 32767.0).astype(np.int16)
    return out_i16.astype(np.float32) / 32768.0


# ---------------------------------------------------------------------------
# WER
# ---------------------------------------------------------------------------

def wer(ref: str, hyp: str) -> float:
    norm = lambda s: re.sub(r"[^a-z0-9' ]", " ", s.lower()).split()
    r, h = norm(ref), norm(hyp)
    n, m = len(r), len(h)
    if n == 0:
        return 0.0 if m == 0 else 1.0
    d = list(range(m + 1))
    for i in range(1, n + 1):
        prev, d[0] = d[0], i
        for j in range(1, m + 1):
            cur  = d[j]
            d[j] = min(d[j] + 1, d[j - 1] + 1, prev + (0 if r[i-1] == h[j-1] else 1))
            prev = cur
    return d[m] / n


# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------

def download_models():
    from huggingface_hub import snapshot_download
    for name, cfg in MODELS.items():
        dest: Path = cfg["local"]
        if (dest / "model.int8.onnx").exists():
            print(f"  {name}: already present at {dest}", flush=True)
            continue
        print(f"  {name}: downloading {cfg['size']} from {cfg['hf_repo']} …", flush=True)
        snapshot_download(repo_id=cfg["hf_repo"], local_dir=str(dest))
        print(f"  {name}: done → {dest}", flush=True)


# ---------------------------------------------------------------------------
# Build recognizer
# ---------------------------------------------------------------------------

def build_recognizer(model_key: str, provider: str = "cpu"):
    import sherpa_onnx

    cfg  = MODELS[model_key]
    mdir = cfg["local"]
    return sherpa_onnx.OfflineRecognizer.from_omnilingual_asr_ctc(
        model=str(mdir / "model.int8.onnx"),
        tokens=str(mdir / "tokens.txt"),
        num_threads=4,
        decoding_method="greedy_search",
        debug=False,
        provider=provider,
    )


# ---------------------------------------------------------------------------
# Run eval on one model
# ---------------------------------------------------------------------------

def run_eval(model_key: str, clips: list[dict], provider: str) -> dict:
    import sherpa_onnx
    print(f"\n  Loading {model_key} ({provider}) …", flush=True)
    t_load = time.perf_counter()
    rec = build_recognizer(model_key, provider=provider)
    load_ms = (time.perf_counter() - t_load) * 1000
    print(f"  Loaded in {load_ms:.0f} ms", flush=True)

    rows = []
    by_group_errors: dict[str, list[float]] = {}

    for e in clips:
        clip_path = Path(e["path"])
        if not clip_path.is_absolute():
            clip_path = ROOT / e["path"]

        gt = e["gt"]
        group = e.get("group", "unknown")

        # Read + resample if needed
        with wave.open(str(clip_path)) as wf:
            raw = np.frombuffer(wf.readframes(wf.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
            file_sr = wf.getframerate()

        if file_sr != SR:
            # simple linear resample via numpy (adequate for benchmarking)
            target_len = int(len(raw) * SR / file_sr)
            raw = np.interp(np.linspace(0, len(raw) - 1, target_len), np.arange(len(raw)), raw).astype(np.float32)

        # Apply adaptive_hot_v2_5 conditioning
        sig = adaptive_hot_v2_5(raw, SR)

        t0   = time.perf_counter()
        stream = rec.create_stream()
        stream.accept_waveform(SR, sig)
        rec.decode_stream(stream)
        decode_ms = (time.perf_counter() - t0) * 1000
        hyp = stream.result.text.strip()

        # strip_before: find GT anchor in hyp (handle leading noise artifacts)
        anchor = e.get("strip_before", "")
        if anchor:
            idx = hyp.lower().find(anchor.lower())
            if idx > 0:
                hyp = hyp[idx:]

        w = wer(gt, hyp)
        by_group_errors.setdefault(group, []).append(w)

        src = e.get("source", "")
        print(f"    [{group:14s}] {src:20s}  wer={w:.3f}  {decode_ms:.0f}ms  {hyp[:60]!r}", flush=True)
        rows.append({"clip": e["clip"], "group": group, "source": src,
                     "duration_s": e.get("duration_s", 0),
                     "decode_ms": round(decode_ms, 1), "wer": w,
                     "gt": gt[:80], "text": hyp[:120]})

    all_wers = [r["wer"] for r in rows]
    overall  = float(np.mean(all_wers)) if all_wers else 1.0
    by_group = {g: float(np.mean(ws)) for g, ws in by_group_errors.items()}
    mean_dec = float(np.mean([r["decode_ms"] for r in rows])) if rows else 0.0

    return {
        "name": model_key,
        "model_dir": str(MODELS[model_key]["local"]),
        "provider": provider,
        "audio_policy": "adaptive_hot_v2_5",
        "load_ms": round(load_ms),
        "clips": len(rows),
        "mean_decode_ms": round(mean_dec, 1),
        "overall_wer": overall,
        "by_group_wer": by_group,
        "rows": rows,
    }


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-download", action="store_true")
    ap.add_argument("--provider", default="cpu", choices=["cpu", "cuda", "dml"])
    args = ap.parse_args()

    # sherpa-onnx valid providers: cpu, dml (DirectML on Windows), cuda (only if ORT GPU build)
    # default to cpu — DML is the Windows GPU path but may not be wired for wav2vec2 CTC
    provider = args.provider

    print(f"\n=== Omnilingual CTC eval  provider={provider} ===\n", flush=True)

    if not args.skip_download:
        print("Downloading models …", flush=True)
        download_models()

    # Load manifest, filter to existing clips only
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    clips = []
    for e in manifest:
        p = Path(e["path"])
        if not p.is_absolute():
            p = ROOT / e["path"]
        if p.exists():
            e = dict(e); e["path"] = str(p)
            clips.append(e)
        else:
            print(f"  skip (missing): {e['source']}", flush=True)

    print(f"\nRunning on {len(clips)}/{len(manifest)} clips (missing = validation audio not on disk)\n", flush=True)

    results = []
    for key in MODELS:
        r = run_eval(key, clips, provider)
        results.append(r)

    # Save results
    out_path = CACHE / "omnilingual_ctc_eval_20260606.json"
    out_path.write_text(json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"\nResults saved → {out_path}", flush=True)

    # -----------------------------------------------------------------------
    # Print comparison table
    # -----------------------------------------------------------------------
    print("\n" + "=" * 72)
    print(f"{'model':22s} {'overall WER':>11}  {'saved_8':>9}  {'mean dec ms':>11}  note")
    print("-" * 72)

    for name, b in BASELINES.items():
        g = b["by_group"]
        s8 = g.get("saved_8", float("nan"))
        print(f"  {name:20s} {b['overall_wer']:>10.1%}  {s8:>9.1%}  {b['mean_decode_ms']:>10}ms  {b['note']}")

    print("-" * 72)
    for r in results:
        g   = r["by_group_wer"]
        s8  = g.get("saved_8", float("nan"))
        note = f"int8 CTC, {r['provider']}, adaptive_hot_v2_5"
        print(f"  {r['name']:20s} {r['overall_wer']:>10.1%}  {s8:>9.1%}  {r['mean_decode_ms']:>10.0f}ms  {note}")

    print("=" * 72)
    print("\nNote: baselines ran on all 18 clips (incl. validation audio); "
          "Omnilingual ran on saved_8 only (10 clips, real-world mic recordings).")
    print("saved_8 WER is the apples-to-apples comparison row.\n")


if __name__ == "__main__":
    main()
