#!/usr/bin/env python3
"""Bakeoff inference adapter — nvidia/nemotron-3.5-asr-streaming-0.6b (transformers-family).

Official inference reference (fetched 2026-07-18):
  https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b
  README.md "Offline transcription" details block:
    AutoModelForRNNT.from_pretrained(...) + AutoProcessor.from_pretrained(...)
    inputs = processor(audio, sampling_rate=..., language="en-US")
    output = model.generate(**inputs, return_dict_in_generate=True)
    processor.decode(output.sequences, skip_special_tokens=True)
  Nemotron3_5Asr is documented as available in Transformers starting v5.13.0
  (installed here: 5.14.1). No NeMo toolkit needed for this checkpoint --
  the repo ships transformers-native config.json/model.safetensors/
  tokenizer.json/processor_config.json alongside the .nemo file, and
  `Nemotron3_5AsrForRNNT` is a registered class in transformers 5.14.1
  (verified via `hasattr(transformers, "Nemotron3_5AsrForRNNT")` before
  writing this adapter).

Contract: ../../CONTRACT.md (SHARED I/O CONTRACT). This adapter is name-blind:
it only ever reads {id, path, duration_s, sha256} manifest rows. It never
sees ground truth, clip identity beyond the opaque id, source ordering, or
any vocabulary/context/bias payload, and it adds none of its own -- only the
model's documented unconditioned offline-transcription call is used.

Roster extension authorization: run-contract.json "roster_extensions"
(2026-07-18, Adrian explicit chat approval, recorded per BAKEOFF_RUN_GATE.md
local lane -- isolation proof + Adrian's explicit approval, both satisfied).
"""
from __future__ import annotations

import argparse
import json
import platform
import sys
import time
import traceback
from pathlib import Path
from typing import Any

# --------------------------------------------------------------------------
# Pinned identity (never silently substituted).
# --------------------------------------------------------------------------
MODEL_KEY = "nemotron-3.5-asr-0.6b"
REPO_ID = "nvidia/nemotron-3.5-asr-streaming-0.6b"
PINNED_REVISION = "f3d333391852ba876df169dcc9ba902d25b6ab0b"
DEFAULT_MODEL_DIR = Path.cwd() / ".cache" / "asr-models" / "nemotron-3.5-asr-streaming-0.6b"
DEFAULT_OUT_DIR = Path.cwd() / "out" / MODEL_KEY

# Decode contract: English is fixed explicitly. This model is prompt-
# conditioned by locale; "en-US" is prompt id 0 (see processor_config.json
# prompt_dictionary), the model's canonical English locale tag -- NOT
# auto-detect, which the decode contract forbids.
FIXED_LANGUAGE = "en-US"

BYTES_PER_PARAM = {"bfloat16": 2, "float16": 2, "float32": 4, "int8": 1}


# ==========================================================================
# Manifest handling (identical shape/contract to the other adapters)
# ==========================================================================
def load_manifest_rows(manifest_path: Path) -> list[dict[str, Any]]:
    data = json.loads(manifest_path.read_text(encoding="utf-8"))
    if isinstance(data, list):
        rows = data
    elif isinstance(data, dict):
        rows = data.get("rows") or data.get("clips") or data.get("items")
        if rows is None:
            raise ValueError(
                f"manifest {manifest_path} is an object but has no "
                "'rows'/'clips'/'items' key"
            )
    else:
        raise ValueError(f"manifest {manifest_path} must be a JSON array or object")

    required = {"id", "path", "duration_s", "sha256"}
    for i, row in enumerate(rows):
        missing = required - set(row)
        if missing:
            raise ValueError(f"manifest row {i} missing required keys: {sorted(missing)}")
    return rows


def select_rows(
    rows: list[dict[str, Any]], smoke_ids: list[str] | None, full: bool
) -> list[dict[str, Any]] | None:
    if smoke_ids and full:
        raise SystemExit("--smoke-ids and --full are mutually exclusive")
    if smoke_ids:
        by_id = {r["id"]: r for r in rows}
        missing = [i for i in smoke_ids if i not in by_id]
        if missing:
            raise SystemExit(f"--smoke-ids not found in manifest: {missing}")
        return [by_id[i] for i in smoke_ids]
    if full:
        return list(rows)
    return None


# ==========================================================================
# Environment capture
# ==========================================================================
def detect_gpu() -> dict[str, Any]:
    try:
        import torch

        if torch.cuda.is_available():
            props = torch.cuda.get_device_properties(0)
            return {
                "gpu": props.name,
                "vram_mb": int(props.total_memory / (1024 * 1024)),
                "driver": getattr(torch.version, "cuda", None),
            }
    except Exception:
        pass
    return {"gpu": None, "vram_mb": None, "driver": None}


def detect_ram_mb() -> int | None:
    try:
        import psutil

        return int(psutil.virtual_memory().total / (1024 * 1024))
    except Exception:
        return None


def host_class(gpu_info: dict[str, Any], ram_mb: int | None) -> str:
    if gpu_info.get("gpu") and gpu_info.get("vram_mb"):
        gpu_name = str(gpu_info["gpu"]).replace(" ", "_")
        return f"{gpu_name}_{round(gpu_info['vram_mb'] / 1024)}GB"
    if ram_mb:
        return f"cpu_{round(ram_mb / 1024)}GB_ram"
    return "unknown_host"


def peak_vram_mb() -> int | None:
    try:
        import torch

        if torch.cuda.is_available():
            return int(torch.cuda.max_memory_allocated() / (1024 * 1024))
    except Exception:
        pass
    return None


# ==========================================================================
# VRAM preflight (coarse; real weights are 600M params @ ~2.4GB bf16/fp32
# mixed on disk -- see acquisition-ledger.json total_bytes for this key)
# ==========================================================================
def preflight_vram_estimate(model_dir: Path, dtype: str) -> dict[str, Any]:
    total = 0
    found = False
    if model_dir.exists():
        for pattern in ("*.safetensors",):
            for f in model_dir.rglob(pattern):
                total += f.stat().st_size
                found = True
    weight_source = "on_disk_weights" if found else "not_acquired"
    total_gb = (total * 1.5) / 1e9  # activation/workspace fudge factor
    if total_gb <= 8 * 0.8:
        disposition = "full"
    elif total_gb <= 8:
        disposition = "smoke"
    else:
        disposition = "defer"
    return {
        "estimated_total_gb": round(total_gb, 2),
        "weight_bytes_on_disk": total,
        "weight_source": weight_source,
        "disposition_8gb": disposition,
        "note": (
            "600M-param cache-aware FastConformer-RNNT; offline mode holds "
            "the whole encoder+decoder resident, no manual KV-cache sizing "
            "needed (RNNT decoder is not autoregressive over a growing "
            "text KV cache the way LLM-family adapters are)."
        ),
    }


# ==========================================================================
# Inference (lazy heavy imports -- never imported for --dry-run)
# ==========================================================================
def resolve_snapshot_dir(model_dir: Path) -> Path:
    snapshots_dir = model_dir / "snapshots"
    if snapshots_dir.exists():
        pinned = snapshots_dir / PINNED_REVISION
        if pinned.exists():
            return pinned
        available = [p.name for p in snapshots_dir.iterdir() if p.is_dir()]
        print(
            f"pinned revision {PINNED_REVISION} not found under {snapshots_dir}; "
            f"available snapshots: {available}. Refusing to silently substitute "
            "a sibling checkpoint.",
            file=sys.stderr,
        )
        raise SystemExit(4)
    return model_dir


def load_model(args: argparse.Namespace):
    import torch
    from transformers import AutoModelForRNNT, AutoProcessor

    dtype = getattr(torch, args.dtype)
    snapshot_dir = resolve_snapshot_dir(args.model_dir)
    processor = AutoProcessor.from_pretrained(str(snapshot_dir))
    model = AutoModelForRNNT.from_pretrained(
        str(snapshot_dir),
        dtype=dtype,
        device_map=args.device if args.device != "cpu" else None,
    )
    if args.device == "cpu":
        model = model.to("cpu")
    model.eval()
    return processor, model


def transcribe_one(
    processor: Any, model: Any, args: argparse.Namespace, row: dict[str, Any]
) -> str:
    """Single documented offline-transcription call (README "Offline
    transcription" block). No manual chunking/stride/overlap -- the
    encoder's max_position_embeddings (5000, subsampling factor 8, ~12.5
    post-subsampling frames/sec) covers well beyond the corpus's longest
    clip (~400s ceiling vs ~111s max clip), so no duration cap is hit for
    this corpus."""
    import torch
    from transformers.audio_utils import load_audio

    sampling_rate = processor.feature_extractor.sampling_rate
    audio = load_audio(row["path"], sampling_rate=sampling_rate)
    inputs = processor(audio, sampling_rate=sampling_rate, language=FIXED_LANGUAGE)
    inputs.to(model.device, dtype=model.dtype)
    with torch.inference_mode():
        output = model.generate(**inputs, return_dict_in_generate=True)
    hyp = processor.decode(output.sequences, skip_special_tokens=True)
    if isinstance(hyp, list):
        hyp = hyp[0] if hyp else ""
    return hyp


# ==========================================================================
# Env record
# ==========================================================================
def build_env_record(args: argparse.Namespace, framework_version: str | None) -> dict[str, Any]:
    gpu_info = detect_gpu()
    ram_mb = detect_ram_mb()
    return {
        "framework": "transformers",
        "framework_version": framework_version,
        "model_revision": PINNED_REVISION,
        "dtype": args.dtype,
        "device": args.device,
        "attn_impl": "model_default",
        "decode_mode": "rnnt_greedy_offline",
        "beam_width": 1,
        "temperature": None,
        "lang_token": FIXED_LANGUAGE,
        "task_token": "transcribe",
        "punct_case_flag": "model_default (native punctuation+capitalization)",
        "max_input_s": None,  # no manual duration cap applied
        "max_new_tokens": None,  # RNNT frame-synchronous decode, not applicable
        "batch_size": 1,
        "vad": "none",
        "waveform_norm": "none",
        "chunk_size_s": None,
        "stride_s": None,
        "overlap_s": None,
        "reassembly": None,
        "timestamps": False,
        "prompt_context_bias": "none",
        "seed": args.seed,
        "non_default_params": {},
        "gpu": gpu_info.get("gpu"),
        "vram_mb": gpu_info.get("vram_mb"),
        "ram": ram_mb,
        "os": platform.platform(),
        "driver": gpu_info.get("driver"),
        "host_class": host_class(gpu_info, ram_mb),
    }


# ==========================================================================
# Row processing: one warm-up (untimed), one retry per row, checkpoint after
# every clip, availability-failure classification (never a substitution).
# ==========================================================================
_COMPLETION_PRIORITY = {
    "ok": 0,
    "empty": 1,
    "truncation": 2,
    "unsupported": 3,
    "crash": 4,
    "oom": 5,
}


def process_rows(
    processor: Any,
    model: Any,
    args: argparse.Namespace,
    rows: list[dict[str, Any]],
    hyp_path: Path,
    status_path: Path,
    cold_load_s: float,
) -> tuple[int, str, int]:
    worst = "ok"
    retries_used = 0
    rows_done = 0

    def bump_worst(candidate: str) -> None:
        nonlocal worst
        if _COMPLETION_PRIORITY[candidate] > _COMPLETION_PRIORITY[worst]:
            worst = candidate

    def write_status() -> None:
        status_path.write_text(
            json.dumps(
                {
                    "completion": worst,
                    "rows_done": rows_done,
                    "rows_total": len(rows),
                    "cold_load_s": round(cold_load_s, 3),
                    "peak_vram_mb": peak_vram_mb(),
                    "retries": retries_used,
                },
                indent=2,
            ),
            encoding="utf-8",
        )

    if rows:
        try:
            transcribe_one(processor, model, args, rows[0])
        except Exception as exc:
            print(f"[warmup] non-fatal warm-up failure: {exc}", file=sys.stderr)

    stop_lane = False
    with hyp_path.open("a", encoding="utf-8") as hyp_f:
        for row in rows:
            if stop_lane:
                break
            attempts = 0
            last_exc: Exception | None = None
            hyp_text = ""
            decode_s: float | None = None
            row_status = "ok"

            while attempts < 2:  # initial attempt + exactly one infra retry
                attempts += 1
                try:
                    t0 = time.perf_counter()
                    hyp_text = transcribe_one(processor, model, args, row)
                    decode_s = time.perf_counter() - t0
                    row_status = "empty" if not hyp_text or not hyp_text.strip() else "ok"
                    last_exc = None
                    break
                except Exception as exc:  # noqa: BLE001 -- classified below
                    last_exc = exc
                    continue

            retries_used = max(retries_used, attempts - 1)

            if last_exc is not None:
                hyp_text = ""
                decode_s = None
                row_status = "unsupported"
                msg = str(last_exc).lower()
                classified = "oom" if ("out of memory" in msg or "cuda oom" in msg) else "crash"
                bump_worst(classified)
                print(
                    f"[row {row['id']}] failed after {attempts} attempt(s): {last_exc}",
                    file=sys.stderr,
                )
                traceback.print_exc(file=sys.stderr)
                if classified == "oom":
                    stop_lane = True
            else:
                bump_worst(row_status)

            hyp_f.write(
                json.dumps(
                    {
                        "id": row["id"],
                        "hypothesis": hyp_text or "",
                        "decode_s": decode_s,
                        "status": row_status,
                    }
                )
                + "\n"
            )
            hyp_f.flush()
            rows_done += 1
            write_status()

    return rows_done, worst, retries_used


# ==========================================================================
# CLI
# ==========================================================================
def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=f"Bakeoff adapter for {REPO_ID}")
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument("--smoke-ids", type=str, default=None, help="comma-separated row ids")
    parser.add_argument("--full", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--device", choices=["cuda", "cpu"], default="cuda")
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    parser.add_argument("--dtype", choices=sorted(BYTES_PER_PARAM), default="bfloat16")
    parser.add_argument("--language", default=FIXED_LANGUAGE, help="fixed explicitly per the decode contract")
    parser.add_argument("--seed", type=int, default=0)
    return parser


def dry_run(args: argparse.Namespace, rows: list[dict[str, Any]]) -> int:
    resolved = {
        "model_key": MODEL_KEY,
        "repo_id": REPO_ID,
        "pinned_revision": PINNED_REVISION,
        "model_dir": str(args.model_dir),
        "model_dir_exists": args.model_dir.exists(),
        "out_dir": str(args.out_dir),
        "manifest": str(args.manifest),
        "manifest_rows": len(rows),
        "device_requested": args.device,
        "dtype": args.dtype,
        "language": args.language,
        "seed": args.seed,
    }
    preflight = preflight_vram_estimate(args.model_dir, args.dtype)
    print(json.dumps({"resolved_config": resolved, "preflight_8gb": preflight}, indent=2))
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    args.smoke_ids = [s.strip() for s in args.smoke_ids.split(",")] if args.smoke_ids else None

    if not args.manifest.exists():
        print(f"manifest not found: {args.manifest}", file=sys.stderr)
        return 2

    rows = load_manifest_rows(args.manifest)

    if args.dry_run:
        return dry_run(args, rows)

    if args.smoke_ids and args.full:
        print("--smoke-ids and --full are mutually exclusive", file=sys.stderr)
        return 2
    selected = select_rows(rows, args.smoke_ids, args.full)
    if selected is None:
        print(
            "a real run requires --smoke-ids or --full (use --dry-run to only validate)",
            file=sys.stderr,
        )
        return 2

    if not args.model_dir.exists():
        print(
            f"model_dir does not exist yet: {args.model_dir} "
            "(acquisition lane has not populated weights; nothing to run)",
            file=sys.stderr,
        )
        return 3

    args.out_dir.mkdir(parents=True, exist_ok=True)
    hyp_path = args.out_dir / "hypotheses.jsonl"
    status_path = args.out_dir / "status.json"
    env_path = args.out_dir / "env.json"
    if hyp_path.exists():
        hyp_path.unlink()

    t_load0 = time.perf_counter()
    processor, model = load_model(args)
    cold_load_s = time.perf_counter() - t_load0

    import transformers

    env_record = build_env_record(args, transformers.__version__)
    env_path.write_text(json.dumps(env_record, indent=2), encoding="utf-8")

    rows_done, worst, retries_used = process_rows(
        processor, model, args, selected, hyp_path, status_path, cold_load_s
    )

    status_path.write_text(
        json.dumps(
            {
                "completion": worst,
                "rows_done": rows_done,
                "rows_total": len(selected),
                "cold_load_s": round(cold_load_s, 3),
                "peak_vram_mb": peak_vram_mb(),
                "retries": retries_used,
            },
            indent=2,
        ),
        encoding="utf-8",
    )

    print(f"done: {rows_done}/{len(selected)} rows, completion={worst}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
