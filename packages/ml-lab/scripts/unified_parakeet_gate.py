"""WER and warm-latency gates for parakeet-unified-en-0.6b finalists."""
from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
import time
from dataclasses import asdict, dataclass, replace
from pathlib import Path

import numpy as np

ROOT = Path("D:/Claude/heardright")
SCRIPT_DIR = ROOT / "tauri-app-next" / "scripts"
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))
if str(ROOT / "tests") not in sys.path:
    sys.path.insert(0, str(ROOT / "tests"))

try:
    from capture_bakeoff.analyze_final_showdown import wer as _wer  # type: ignore  # noqa: E402
except ModuleNotFoundError:
    def _wer(ref: str, hyp: str) -> float:
        r = ref.split()
        h = hyp.split()
        prev = list(range(len(h) + 1))
        for i, rw in enumerate(r, start=1):
            cur = [i]
            for j, hw in enumerate(h, start=1):
                cur.append(
                    min(
                        prev[j] + 1,
                        cur[j - 1] + 1,
                        prev[j - 1] + (0 if rw == hw else 1),
                    )
                )
            prev = cur
        return prev[-1] / max(1, len(r))
from unified_parakeet_hybrid_matrix import (  # noqa: E402
    BLANK,
    SR,
    Variant,
    _ensure_sentencepiece,
    _provider,
    _session_options,
    _static_chunks,
    decode_tokens,
    load_clip,
    model_disk_mb,
    preprocess,
    variants,
)

EVAL = ROOT / "tests" / "asr_global_eval"
LAT_CLIPS = [
    ("2s", ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "long_paragraph_first2s.wav"),
    ("10s", ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_10s.wav"),
    ("60s", ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_60s.wav"),
    ("150s", ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_150s.wav"),
]
TAG = re.compile(r"<\|[^|]*\|>")
PUNCT = re.compile(r"[^a-z0-9' ]")

DEFAULT_FINALISTS = [
    "int8enc_int8dec__enc_dml__dec_cpu",
    "int8enc_int8dec__enc_dml__dec_dml",
    "fp16naiveenc_int8dec__enc_dml__dec_cpu",
    "mixedenc_int8dec__enc_dml__dec_cpu",
    "fp32enc_int8dec__enc_dml__dec_cpu",
    "fp32enc_fp32dec__enc_dml__dec_cpu",
    "fp32enc_fp32dec__enc_dml__dec_dml",
]


@dataclass
class WerRow:
    clip: str
    group: str
    duration_s: float
    wer: float | None
    hyp: str
    latency_s: float | None
    error: str = ""


def _self_test() -> None:
    rows = [
        {"source_order": 12, "file": "a.wav", "authority": "Hello.", "mode": "app"},
        {"source_order": 13, "clip": "b.wav", "gt": "World.", "group": "short", "duration_s": 1.5},
        {"source_order": 14, "file": "c.wav", "asr": "From mapped clips."},
    ]
    selected = select_manifest_rows(rows, "12")
    assert len(selected) == 1
    assert clip_name(selected[0]) == "a.wav"
    assert reference_text(selected[0]) == "Hello."
    assert item_group(selected[0]) == "app"
    assert clip_name(rows[1]) == "b.wav"
    assert reference_text(rows[1]) == "World."
    assert item_group(rows[1]) == "short"
    assert reference_text(rows[2]) == "From mapped clips."


def source_order_filter(raw: str | None) -> set[int] | None:
    if not raw:
        return None
    return {int(part.strip()) for part in raw.split(",") if part.strip()}


def select_manifest_rows(rows: list[dict], raw_source_orders: str | None) -> list[dict]:
    keep = source_order_filter(raw_source_orders)
    if keep is None:
        return rows
    return [row for row in rows if int(row.get("source_order", -1)) in keep]


def clip_name(item: dict) -> str:
    return str(item.get("clip") or item["file"])


def reference_text(item: dict) -> str:
    return str(item.get("gt") or item.get("authority") or item.get("raw_asr") or item.get("asr") or "")


def item_group(item: dict) -> str:
    return str(item.get("group") or item.get("mode") or "default")


def item_duration(item: dict, audio: np.ndarray) -> float:
    raw = item.get("duration_s")
    return float(raw) if raw is not None else float(audio.shape[0] / SR)


class UnifiedRunner:
    def __init__(self, variant: Variant):
        import onnxruntime as rt

        self.variant = variant
        self.spm = _ensure_sentencepiece()
        self.encoder = rt.InferenceSession(
            str(variant.encoder),
            sess_options=_session_options(variant.encoder_provider, variant.static_window),
            providers=_provider(variant.encoder_provider),
        )
        self.decoder = rt.InferenceSession(
            str(variant.decoder),
            sess_options=_session_options(variant.decoder_provider),
            providers=_provider(variant.decoder_provider),
        )
        self.tokenizer = self.spm.SentencePieceProcessor(model_file=str(variant.tokenizer))

    @property
    def providers(self) -> dict[str, list[str]]:
        return {
            "encoder": self.encoder.get_providers(),
            "decoder": self.decoder.get_providers(),
        }

    def transcribe(self, audio: np.ndarray) -> str:
        feats, feat_lens = preprocess(audio)
        texts: list[str] = []
        chunks = _static_chunks(feats, feat_lens, self.variant.static_window) if self.variant.static_window else [(feats, feat_lens)]
        for chunk_feats, chunk_lens in chunks:
            enc_out, enc_len = self.encoder.run(
                ["outputs", "encoded_lengths"],
                {"audio_signal": chunk_feats, "length": chunk_lens},
            )
            if not np.isfinite(enc_out).all():
                raise ValueError("encoder output contains NaN/Inf")
            text = self.tokenizer.decode(decode_tokens(self.decoder, enc_out, enc_len, self.variant.max_tokens)).strip()
            if text:
                texts.append(text)
        return " ".join(texts).strip()


def norm(text: str) -> str:
    text = TAG.sub(" ", (text or "").lower())
    text = PUNCT.sub(" ", text)
    return " ".join(text.split())


def variant_by_name(name: str) -> Variant:
    max_tokens: int | None = None
    if "@" in name:
        name, raw_max_tokens = name.rsplit("@", 1)
        max_tokens = int(raw_max_tokens)
    by_name = {v.name: v for v in variants(include_mixed=True)}
    if name not in by_name:
        raise SystemExit(f"unknown variant {name}; known={sorted(by_name)}")
    variant = by_name[name]
    if max_tokens is not None:
        variant = replace(variant, name=f"{variant.name}@{max_tokens}", max_tokens=max_tokens)
    return variant


def warm_runner(runner: UnifiedRunner, clip: Path | None = None) -> None:
    warm_clip = clip or (EVAL / "clips" / "libri_short_1272_1272-128104-0000.wav")
    if not warm_clip.exists():
        return
    audio = load_clip(warm_clip)
    try:
        runner.transcribe(audio)
    except Exception:
        pass


def run_wer(
    variant_name: str,
    *,
    manifest_path: Path = EVAL / "manifest.json",
    clips_dir: Path = EVAL / "clips",
    source_orders: str | None = None,
) -> dict[str, object]:
    variant = variant_by_name(variant_name)
    tag_base = manifest_path.stem.replace(" ", "_")
    tag_parent = manifest_path.parent.name.replace(" ", "_")
    tag = f"{tag_parent}__{tag_base}" if tag_parent and tag_parent not in {"asr_global_eval", "scripts"} else tag_base
    safe_name = f"{variant.name.replace('@', '_maxtok')}__{tag}"
    out_path = SCRIPT_DIR / f"_unified_wer_{safe_name}.json"
    manifest = select_manifest_rows(json.loads(manifest_path.read_text(encoding="utf-8")), source_orders)
    runner = UnifiedRunner(variant)
    warm_runner(runner, clips_dir / clip_name(manifest[0]) if manifest else None)
    rows: list[WerRow] = []
    summary: dict[str, object] = {
        "variant": variant_name,
        "manifest": str(manifest_path),
        "clips_dir": str(clips_dir),
        "source_orders": sorted(source_order_filter(source_orders) or []),
        "providers": runner.providers,
        "disk_mb": model_disk_mb((variant.encoder, variant.decoder, variant.tokenizer)),
        "rows": [],
    }

    for item in manifest:
        clip = clips_dir / clip_name(item)
        t0 = time.perf_counter()
        try:
            audio = load_clip(clip)
            hyp = runner.transcribe(audio)
            latency = time.perf_counter() - t0
            w = _wer(norm(reference_text(item)), norm(hyp))
            duration_s = item_duration(item, audio)
            row = WerRow(clip_name(item), item_group(item), duration_s, round(w, 4), hyp[:400], round(latency, 3))
            print(f"{variant_name} {row.group:<12} {duration_s:>6.1f}s WER={w*100:>5.1f}% {row.clip}", flush=True)
        except Exception as exc:
            row = WerRow(
                clip_name(item),
                item_group(item),
                float(item.get("duration_s") or 0.0),
                None,
                "",
                None,
                f"{type(exc).__name__}: {exc}",
            )
            print(f"{variant_name} ERROR {clip_name(item)} {row.error}", flush=True)
        rows.append(row)
        summary["rows"] = [asdict(r) for r in rows]
        out_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")

    def agg(group: str | None = None) -> float | None:
        vals = [r.wer for r in rows if r.wer is not None and (group is None or r.group == group)]
        return round(sum(vals) / len(vals) * 100, 2) if vals else None

    summary["overall_wer"] = agg()
    summary["by_group"] = {g: agg(g) for g in sorted({r.group for r in rows})}
    out_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(f"{variant_name} OVERALL={summary['overall_wer']} by_group={summary['by_group']}", flush=True)
    return summary


def run_latency(variant_names: list[str], runs: int) -> list[dict[str, object]]:
    out_path = SCRIPT_DIR / "_unified_latency_finalists.json"
    results: list[dict[str, object]] = []
    for variant_name in variant_names:
        variant = variant_by_name(variant_name)
        runner = UnifiedRunner(variant)
        warm_runner(runner)
        row: dict[str, object] = {
            "variant": variant_name,
            "providers": runner.providers,
            "disk_mb": model_disk_mb((variant.encoder, variant.decoder, variant.tokenizer)),
            "clips": {},
        }
        for label, clip in LAT_CLIPS:
            audio = load_clip(clip)
            times: list[float] = []
            text = ""
            for _ in range(runs):
                t0 = time.perf_counter()
                text = runner.transcribe(audio)
                times.append(time.perf_counter() - t0)
            item = {
                "duration_s": round(audio.shape[0] / SR, 3),
                "median_s": round(statistics.median(times), 3),
                "runs_s": [round(t, 3) for t in times],
                "text": text[:220],
            }
            row["clips"][label] = item
            out_path.write_text(json.dumps(results + [row], indent=2), encoding="utf-8")
            print(f"{variant_name:<42} {label:<4} {item['median_s']:.3f}s", flush=True)
        results.append(row)
        out_path.write_text(json.dumps(results, indent=2), encoding="utf-8")
    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=("wer", "latency", "all"), required=True)
    ap.add_argument("--variants", default=",".join(DEFAULT_FINALISTS))
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--manifest", type=Path, default=EVAL / "manifest.json")
    ap.add_argument("--clips-dir", type=Path, default=EVAL / "clips")
    ap.add_argument("--source-orders")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        _self_test()
        print("self-test ok")
        return 0

    variant_names = [v.strip() for v in args.variants.split(",") if v.strip()]
    if args.mode in ("latency", "all"):
        run_latency(variant_names, runs=args.runs)
    if args.mode in ("wer", "all"):
        for name in variant_names:
            run_wer(
                name,
                manifest_path=args.manifest,
                clips_dir=args.clips_dir,
                source_orders=args.source_orders,
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
