"""Cohere Transcribe quantization smoke — GPU tier feasibility.

Question: can Cohere run as a high-accuracy tier for good GPUs at a smaller
on-disk/VRAM footprint than the 4.13 GB bf16 checkpoint?

This is a SMOKE, not a scored bakeoff run. It scores a fixed subset and reports
the *delta* between precisions measured on that identical subset, so the
comparison is internally valid even though the absolute numbers are not the
locked corpus figures (bf16 full-corpus reference: 4.94%).

Precisions:
  bf16  — the reference load path (BYTES_PER_PARAM 2, ~4.13 GB weights)
  int8  — bitsandbytes LLM.int8 (~2.07 GB)
  nf4   — bitsandbytes NF4 4-bit, double-quantized (~1.03 GB)

nf4 here is a *quality* proxy for the ONNX `q4f16` artifact (1.46 GB on disk):
both are 4-bit weight quantization of the same checkpoint. It is not a proxy for
that artifact's speed — bitsandbytes 4-bit is compute-inefficient, while ONNX
MatMulNBits has real CUDA kernels. Read the WER column, not the RTFx column, when
judging the ONNX path.

    .venv-infer/Scripts/python.exe cohere_quant_smoke.py \
        --canonical-dir canonical-eval --limit 30 --precisions bf16 nf4
"""

from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

DEFAULT_MODEL_DIR = Path("D:/Claude/heardright/.cache/asr-models/cohere-transcribe")


def load_subset(canonical_dir: Path, limit: int) -> list[dict]:
    """Stratified by group, plus every long-form clip (quant hurts long decode first)."""
    candidates = json.loads(
        (canonical_dir / "candidate_manifest.json").read_text(encoding="utf-8")
    )
    truth = json.loads(
        (canonical_dir / "_scorer_private" / "id_to_truth.json").read_text(
            encoding="utf-8"
        )
    )
    by_group: dict[str, list[dict]] = {}
    for row in candidates:
        by_group.setdefault(truth[row["id"]]["group"], []).append(row)

    picked: list[dict] = []
    long_form = {"productivity_long_form", "dictation_tts_long_form"}
    for group, rows in by_group.items():
        rows.sort(key=lambda r: r["id"])
        if group in long_form:
            picked.extend(rows)  # all of them
        else:
            step = max(1, len(rows) // max(1, limit // len(by_group)))
            picked.extend(rows[::step][: max(1, limit // len(by_group))])
    for row in picked:
        row["ground_truth"] = truth[row["id"]]["ground_truth"]
        row["group"] = truth[row["id"]]["group"]
    return picked


def build_model(model_dir: Path, precision: str):
    import torch
    from transformers import AutoProcessor, CohereAsrForConditionalGeneration

    kwargs: dict = {"device_map": "cuda:0"}
    if precision == "bf16":
        kwargs["dtype"] = torch.bfloat16
    else:
        from transformers import BitsAndBytesConfig

        if precision == "int8":
            kwargs["quantization_config"] = BitsAndBytesConfig(load_in_8bit=True)
        elif precision == "nf4":
            kwargs["quantization_config"] = BitsAndBytesConfig(
                load_in_4bit=True,
                bnb_4bit_quant_type="nf4",
                bnb_4bit_use_double_quant=True,
                bnb_4bit_compute_dtype=torch.bfloat16,
            )
        else:
            raise ValueError(f"unknown precision: {precision}")
    processor = AutoProcessor.from_pretrained(str(model_dir))
    model = CohereAsrForConditionalGeneration.from_pretrained(str(model_dir), **kwargs)
    model.eval()
    return processor, model


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--canonical-dir", required=True, type=Path)
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    parser.add_argument("--limit", type=int, default=30)
    parser.add_argument(
        "--precisions", nargs="+", default=["bf16", "nf4"],
        choices=["bf16", "int8", "nf4"],
    )
    parser.add_argument("--out", type=Path)
    parser.add_argument(
        "--emit-dir",
        type=Path,
        help="write <precision>/hypotheses.jsonl for scoring with score-asr-wer.mjs, "
        "so quantized runs are scored by the same frozen scorer as every other engine",
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        help="score this manifest verbatim instead of the stratified subset",
    )
    args = parser.parse_args()

    import torch
    from transformers.audio_utils import load_audio

    import importlib.util
    import sys

    spec = importlib.util.spec_from_file_location(
        "verifier",
        Path(__file__).parent / "productivity-corpus" / "verify_tts_intelligibility.py",
    )
    verifier = importlib.util.module_from_spec(spec)
    sys.modules["verifier"] = verifier
    spec.loader.exec_module(verifier)

    if args.manifest:
        truth = json.loads(
            (args.canonical_dir / "_scorer_private" / "id_to_truth.json").read_text(
                encoding="utf-8"
            )
        )
        rows = json.loads(args.manifest.read_text(encoding="utf-8"))
        for row in rows:
            row["ground_truth"] = truth[row["id"]]["ground_truth"]
            row["group"] = truth[row["id"]]["group"]
    else:
        rows = load_subset(args.canonical_dir, args.limit)
    audio_s = sum(r["duration_s"] for r in rows)
    print(f"scoring {len(rows)} clips, {audio_s/60:.1f} min audio", flush=True)

    results: dict[str, dict] = {}
    for precision in args.precisions:
        torch.cuda.empty_cache()
        torch.cuda.reset_peak_memory_stats()
        load_start = time.time()
        processor, model = build_model(args.model_dir, precision)
        load_s = time.time() - load_start

        total_err = 0
        total_ref = 0
        synth_s = 0.0
        per_group: dict[str, list[int]] = {}
        hypotheses: list[dict] = []
        for row in rows:
            audio = load_audio(row["path"], sampling_rate=16000)
            inputs = processor(
                audio, sampling_rate=16000, return_tensors="pt", language="en"
            )
            inputs = inputs.to(model.device, dtype=model.dtype)
            budget = max(64, int(row["duration_s"] * 6) + 64)
            start = time.time()
            with torch.no_grad():
                out = model.generate(**inputs, max_new_tokens=budget)
            synth_s += time.time() - start
            text = processor.decode(out, skip_special_tokens=True)
            if isinstance(text, list):
                text = " ".join(text)
            # Key must be "hypothesis": score-asr-wer.mjs reads that field and
            # silently scores a missing one as an empty string (100% WER).
            hypotheses.append({"id": row["id"], "hypothesis": text, "status": "ok"})
            ref = verifier.normalize(row["ground_truth"])
            hyp = verifier.normalize(text)
            rate, _, _, _ = verifier.wer(ref, hyp)
            errs = round(rate * len(ref))
            total_err += errs
            total_ref += len(ref)
            per_group.setdefault(row["group"], [0, 0])
            per_group[row["group"]][0] += errs
            per_group[row["group"]][1] += len(ref)

        if args.emit_dir:
            target = args.emit_dir / precision
            target.mkdir(parents=True, exist_ok=True)
            (target / "hypotheses.jsonl").write_text(
                "".join(json.dumps(h) + "\n" for h in hypotheses), encoding="utf-8"
            )
            print(f"  wrote {target / 'hypotheses.jsonl'}", flush=True)

        peak_gb = torch.cuda.max_memory_allocated() / 1e9
        wer = total_err / max(1, total_ref)
        results[precision] = {
            "wer": round(wer, 4),
            "rtfx": round(audio_s / synth_s, 2),
            "peak_vram_gb": round(peak_gb, 2),
            "load_s": round(load_s, 1),
            "by_group": {
                g: round(e / max(1, n), 4) for g, (e, n) in sorted(per_group.items())
            },
        }
        print(
            f"{precision:5s} WER={wer:6.2%} RTFx={audio_s/synth_s:6.2f} "
            f"peakVRAM={peak_gb:4.2f} GB load={load_s:.1f}s",
            flush=True,
        )
        del model, processor
        torch.cuda.empty_cache()

    if "bf16" in results:
        base = results["bf16"]["wer"]
        print("\ndelta vs bf16 on this subset:")
        for precision, row in results.items():
            if precision == "bf16":
                continue
            print(
                f"  {precision:5s} {row['wer']:.2%} vs {base:.2%} "
                f"-> {(row['wer']-base)*100:+.2f} pts "
                f"({(row['wer']/base-1)*100:+.1f}% relative)"
            )
    if args.out:
        args.out.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
