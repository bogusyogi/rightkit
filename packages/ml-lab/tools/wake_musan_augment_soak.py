#!/usr/bin/env python
"""MUSAN-augment local mic-soak wake/false-fire captures."""

from __future__ import annotations

import argparse
import hashlib
import json
import random
from pathlib import Path

import numpy as np
import soundfile as sf

SAMPLE_RATE = 16_000
SNRS_DB = [5.0, 10.0, 15.0]


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def stable_id(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()


def read_mono(path: Path) -> tuple[np.ndarray, int]:
    audio, sr = sf.read(str(path), dtype="float32", always_2d=False)
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    return np.asarray(audio, dtype="float32"), int(sr)


def resample_if_needed(audio: np.ndarray, sr: int) -> np.ndarray:
    if sr == SAMPLE_RATE:
        return audio.astype("float32", copy=False)
    try:
        from scipy.signal import resample_poly
    except Exception as exc:
        raise RuntimeError("scipy is required to resample non-16k sources") from exc
    import math

    gcd = math.gcd(int(sr), SAMPLE_RATE)
    return resample_poly(audio, SAMPLE_RATE // gcd, int(sr) // gcd).astype("float32")


def rms(audio: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.square(audio.astype("float64"))) + 1e-12))


def mix_at_snr(clean: np.ndarray, noise: np.ndarray, snr_db: float, rng: random.Random) -> np.ndarray:
    if noise.size < clean.size:
        reps = int(np.ceil(clean.size / max(1, noise.size)))
        noise = np.tile(noise, reps)
    if noise.size > clean.size:
        start = rng.randrange(0, noise.size - clean.size + 1)
        noise = noise[start : start + clean.size]
    target_noise_rms = rms(clean) / (10.0 ** (snr_db / 20.0))
    scaled_noise = noise * (target_noise_rms / max(rms(noise), 1e-6))
    mixed = clean + scaled_noise
    peak = float(np.max(np.abs(mixed))) if mixed.size else 0.0
    if peak > 0.98:
        mixed = mixed * (0.98 / peak)
    return mixed.astype("float32")


def load_soak_rows(path: Path) -> list[dict]:
    rows = []
    for row in iter_jsonl(path):
        fixture = str(row.get("fixture_group", ""))
        wav = Path(str(row.get("path", "")))
        if fixture.startswith("live_fire") and wav.exists():
            rows.append(row)
    return rows


def load_musan_paths(path: Path, max_sources: int) -> list[Path]:
    paths: list[Path] = []
    seen: set[str] = set()
    for row in iter_jsonl(path):
        bucket = str(row.get("source_bucket", "")).lower()
        raw_path = str(row.get("path", "") or "")
        if not raw_path.strip():
            continue
        wav = Path(raw_path)
        if wav == Path("."):
            continue
        if "musan" not in bucket or not wav.exists():
            continue
        key = str(wav).lower()
        if key in seen:
            continue
        seen.add(key)
        paths.append(wav)
        if len(paths) >= max_sources:
            break
    return paths


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expansion-root", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607"))
    parser.add_argument("--eval-stress-manifest", type=Path, default=Path(r"D:\Claude\heardright\train\campaigns\adaptive_reset_20260606_v1\manifests\eval_stress_manifest.jsonl"))
    parser.add_argument("--corpus-master", type=Path, default=Path(r"D:\Claude\heardright\train\campaigns\adaptive_reset_20260606_v1\manifests\corpus_master.jsonl"))
    parser.add_argument("--max-musan-sources", type=int, default=240)
    parser.add_argument("--augments-per-soak", type=int, default=6)
    parser.add_argument("--seed", type=int, default=20260607)
    args = parser.parse_args()

    rng = random.Random(args.seed)
    soak_rows = load_soak_rows(args.eval_stress_manifest)
    musan_paths = load_musan_paths(args.corpus_master, args.max_musan_sources)
    if not soak_rows:
        raise RuntimeError("No local live_fire soak rows found")
    if not musan_paths:
        raise RuntimeError("No local MUSAN paths found")

    out_root = args.expansion_root / "raw" / "mic_soak_musan_aug"
    manifest = args.expansion_root / "manifests" / "mic_soak_musan_aug_manifest.jsonl"
    report = args.expansion_root / "reports" / "mic_soak_musan_aug_summary.json"
    out_root.mkdir(parents=True, exist_ok=True)
    manifest.parent.mkdir(parents=True, exist_ok=True)

    noise_cache: dict[Path, np.ndarray] = {}
    rows_written = 0
    skipped_noise: list[dict] = []
    with manifest.open("w", encoding="utf-8", newline="\n") as out:
        for soak in soak_rows:
            clean_path = Path(soak["path"])
            clean, clean_sr = read_mono(clean_path)
            clean = resample_if_needed(clean, clean_sr)
            choices = rng.sample(musan_paths, k=min(args.augments_per_soak, len(musan_paths)))
            for idx, noise_path in enumerate(choices):
                if noise_path not in noise_cache:
                    try:
                        noise_audio, noise_sr = read_mono(noise_path)
                        noise_cache[noise_path] = resample_if_needed(noise_audio, noise_sr)
                    except Exception as exc:
                        skipped_noise.append({"path": str(noise_path), "error": str(exc)[:500]})
                        continue
                snr_db = SNRS_DB[idx % len(SNRS_DB)]
                mixed = mix_at_snr(clean, noise_cache[noise_path], snr_db, rng)
                row_id = stable_id(f"{clean_path}|{noise_path}|{snr_db}|{idx}")
                label = int(soak.get("label") or 0)
                semantic = "zephyr_positive_musan_aug" if label == 1 else "near_phonetic_musan_aug"
                dst = out_root / semantic / f"{row_id}.wav"
                dst.parent.mkdir(parents=True, exist_ok=True)
                sf.write(str(dst), mixed, SAMPLE_RATE, subtype="PCM_16")
                row = {
                    "schema_version": 1,
                    "id": row_id,
                    "path": str(dst),
                    "label": label,
                    "word": "zephyr" if label == 1 else "",
                    "source_bucket": "mic_soak_musan_aug",
                    "semantic_bucket": semantic,
                    "source_kind": "mic_soak_musan_aug",
                    "false_fire_type": soak.get("false_fire_type", "not_applicable"),
                    "source_path": str(clean_path),
                    "musan_path": str(noise_path),
                    "snr_db": snr_db,
                    "eligible_for_train": True,
                    "eligible_for_eval": False,
                    "eligible_for_mine": True,
                    "partition_hint": "unified_candidate",
                    "license_or_provenance": "internal_mic_soak_plus_local_musan_aug",
                }
                out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
                rows_written += 1

    summary = {
        "soak_rows": len(soak_rows),
        "musan_sources": len(musan_paths),
        "augments_per_soak": args.augments_per_soak,
        "rows_written": rows_written,
        "skipped_noise_rows": len(skipped_noise),
        "skipped_noise_sample": skipped_noise[:20],
        "manifest": str(manifest),
        "output_root": str(out_root),
    }
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
