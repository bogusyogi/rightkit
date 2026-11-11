#!/usr/bin/env python3
"""Bakeoff inference adapter — Qwen3-ASR-1.7B-hf (transformers-family).

Official inference reference (fetched 2026-07-17):
  https://huggingface.co/Qwen/Qwen3-ASR-1.7B-hf
  AutoProcessor.apply_transcription_request(...) + AutoModelForMultimodalLM.generate(...).
See README.md in this directory for the exact source excerpt and the
long-form / duration-cap caveat.

Contract: ../../CONTRACT.md (SHARED I/O CONTRACT). This adapter is name-blind:
it only ever reads {id, path, duration_s, sha256} manifest rows. It never
sees ground truth, clip identity beyond the opaque id, source ordering, or
any vocabulary/context/bias payload, and it adds none of its own -- only the
model's documented unconditioned ASR call is used.

This file authors code only. It does NOT install packages, download
weights, or run inference by itself -- execution is gated on the frozen run
packet's approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) and on the
acquisition lane having populated --model-dir.
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
# Pinned identity (never silently substituted -- see docs/plans/2026-07-17-
# open-asr-top-model-dispatch-run-packet.md "Exact artifact pins").
# --------------------------------------------------------------------------
MODEL_KEY = "qwen3-asr-1.7b"
REPO_ID = "Qwen/Qwen3-ASR-1.7B-hf"
PINNED_REVISION = "057a3b044fcd31c433e7971ab40d68d20e7eae6d"
DEFAULT_MODEL_DIR = Path.cwd() / ".cache" / "asr-models" / "qwen3-asr-1.7b"
DEFAULT_OUT_DIR = Path.cwd() / "out" / MODEL_KEY

# Contract's declared maximum smoke-clip duration (librispeech_032_long120.wav).
MAX_AUDIO_S_CONTRACT = 110.75

# --------------------------------------------------------------------------
# Long-form / cap caveat (see README.md for the full writeup).
#
# HR's own prior finding (2026-05-25, docs/OWN_MODEL_PATH_RESEARCH_2026-07-17.md
# section 6a): the 0.6B sibling (sherpa runtime, non-streaming whole-utterance
# mode) had an effective ~20 s input cap traced to a 512-token limit. The
# 1.7B-hf card claims the model can "handle long audio" via a single
# apply_transcription_request() call with no manual chunking documented, but
# gives no numeric cap. We do NOT invent chunking for this adapter -- we call
# the documented single-shot path and instrument for truncation instead of
# pre-emptively rejecting or splitting long clips.
# --------------------------------------------------------------------------
QWEN_LEGACY_SIBLING_CAP_S = 20.0  # 0.6B sherpa cap, carried only as a documented risk flag

MAX_NEW_TOKENS_FLOOR = 256
MAX_NEW_TOKENS_CEILING = 1024
MAX_NEW_TOKENS_PER_SEC = 6.0  # rough dictation-rate padding factor; see README

# --------------------------------------------------------------------------
# VRAM preflight constants. Prefer real on-disk weight bytes and a parsed
# config.json when the acquisition lane has populated --model-dir; fall back
# to a documented, explicitly-flagged approximate placeholder otherwise.
# --------------------------------------------------------------------------
BYTES_PER_PARAM = {"bfloat16": 2, "float16": 2, "float32": 4, "int8": 1}
APPROX_PARAM_COUNT_BILLION = 1.7  # from the model name; unverified exact count
FALLBACK_HIDDEN_SIZE = 2048  # Qwen3-1.7B-class placeholder, documented-uncertain
FALLBACK_NUM_LAYERS = 28  # Qwen3-1.7B-class placeholder, documented-uncertain
ENCODER_FRAMES_PER_SEC = 12.5  # approx post-subsampling encoder rate; documented-uncertain
ACTIVATION_OVERHEAD_MULTIPLIER = 1.5  # workspace/attention-buffer fudge factor


# ==========================================================================
# Manifest handling
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
# VRAM preflight estimator
# ==========================================================================
def load_config_geometry(model_dir: Path) -> tuple[int, int, str]:
    """Prefer the real config.json under model_dir; fall back to a documented
    placeholder geometry when weights are not yet acquired."""
    cfg_path = model_dir / "config.json"
    if cfg_path.exists():
        try:
            cfg = json.loads(cfg_path.read_text(encoding="utf-8"))

            # Prefer a named decoder/text sub-config over encoder/projector
            # sub-configs. A naive recursive first-match scan can be shadowed
            # by a small audio-projector Q-Former-style block (e.g. Granite
            # Speech's projector_config also declares hidden_size/
            # num_hidden_layers, but those are NOT the LLM decoder's real
            # geometry that actually dominates KV-cache size).
            for key in ("text_config", "decoder_config", "llm_config"):
                sub = cfg.get(key)
                if isinstance(sub, dict) and isinstance(sub.get("hidden_size"), int):
                    hidden = sub["hidden_size"]
                    layers = sub.get("num_hidden_layers") or sub.get("num_layers") or FALLBACK_NUM_LAYERS
                    return hidden, layers, f"config.json:{key}"

            if isinstance(cfg.get("hidden_size"), int):
                hidden = cfg["hidden_size"]
                layers = cfg.get("num_hidden_layers") or cfg.get("num_layers") or FALLBACK_NUM_LAYERS
                return hidden, layers, "config.json:root"

            # Last resort: generic recursive first-match scan (coarse; may
            # hit a non-decoder sub-config -- see note above).
            def find(key: str, node: Any) -> int | None:
                if isinstance(node, dict):
                    if key in node and isinstance(node[key], int):
                        return node[key]
                    for value in node.values():
                        found = find(key, value)
                        if found is not None:
                            return found
                return None

            hidden = find("hidden_size", cfg) or FALLBACK_HIDDEN_SIZE
            layers = find("num_hidden_layers", cfg) or find("num_layers", cfg) or FALLBACK_NUM_LAYERS
            return hidden, layers, "config.json:recursive_fallback"
        except Exception:
            pass
    return FALLBACK_HIDDEN_SIZE, FALLBACK_NUM_LAYERS, "fallback_placeholder"


def estimate_weight_bytes(model_dir: Path, dtype: str) -> tuple[int, str]:
    """Prefer real on-disk weight bytes; fall back to an approximate
    param-count * bytes-per-param estimate when weights are not present."""
    total = 0
    found = False
    if model_dir.exists():
        for pattern in ("*.safetensors", "*.bin", "*.pt"):
            for f in model_dir.rglob(pattern):
                total += f.stat().st_size
                found = True
    if found and total > 0:
        return total, "on_disk_weights"
    approx = int(APPROX_PARAM_COUNT_BILLION * 1e9 * BYTES_PER_PARAM.get(dtype, 2))
    return approx, "approx_param_count_placeholder"


def preflight_vram_estimate(
    model_dir: Path, dtype: str, max_audio_s: float, max_new_tokens: int
) -> dict[str, Any]:
    weight_bytes, weight_source = estimate_weight_bytes(model_dir, dtype)
    hidden, layers, geometry_source = load_config_geometry(model_dir)
    bytes_elem = BYTES_PER_PARAM.get(dtype, 2)

    encoder_frames = max_audio_s * ENCODER_FRAMES_PER_SEC
    seq_len = encoder_frames + max_new_tokens
    kv_bytes = 2 * layers * hidden * bytes_elem * seq_len  # 2x for K & V

    total_bytes = (weight_bytes + kv_bytes) * ACTIVATION_OVERHEAD_MULTIPLIER
    total_gb = total_bytes / 1e9

    if total_gb <= 8 * 0.8:
        disposition = "full"
    elif total_gb <= 8:
        disposition = "smoke"
    else:
        disposition = "defer"

    return {
        "estimated_total_gb": round(total_gb, 2),
        "weight_gb": round(weight_bytes / 1e9, 2),
        "weight_source": weight_source,
        "kv_activation_gb": round((kv_bytes * ACTIVATION_OVERHEAD_MULTIPLIER) / 1e9, 2),
        "assumptions": {
            "hidden_size": hidden,
            "num_layers": layers,
            "geometry_source": geometry_source,
            "encoder_frames_per_sec": ENCODER_FRAMES_PER_SEC,
            "max_audio_s": max_audio_s,
            "max_new_tokens": max_new_tokens,
            "activation_overhead_multiplier": ACTIVATION_OVERHEAD_MULTIPLIER,
            "note": (
                "hidden_size/num_layers come from config.json when --model-dir "
                "is populated; otherwise they are documented-uncertain "
                "placeholders and the estimate should be treated as coarse."
            ),
        },
        "disposition_8gb": disposition,
    }


# ==========================================================================
# Inference (lazy heavy imports -- never imported for --dry-run)
# ==========================================================================
def compute_max_new_tokens(duration_s: float) -> int:
    est = int(duration_s * MAX_NEW_TOKENS_PER_SEC)
    return max(MAX_NEW_TOKENS_FLOOR, min(MAX_NEW_TOKENS_CEILING, est))


def resolve_snapshot_dir(model_dir: Path) -> Path:
    """If --model-dir looks like a huggingface_hub cache root (contains
    snapshots/<revision>/...), resolve to the exact pinned snapshot and
    refuse to silently substitute a different revision. Otherwise assume
    --model-dir already IS the pinned snapshot (the acquisition lane's
    documented layout)."""
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
    from transformers import AutoModelForMultimodalLM, AutoProcessor

    dtype = getattr(torch, args.dtype)
    snapshot_dir = resolve_snapshot_dir(args.model_dir)
    processor = AutoProcessor.from_pretrained(str(snapshot_dir))
    model = AutoModelForMultimodalLM.from_pretrained(
        str(snapshot_dir),
        dtype=dtype,
        device_map=args.device if args.device != "cpu" else None,
        attn_implementation=args.attn_impl,
    )
    if args.device == "cpu":
        model = model.to("cpu")
    model.eval()
    return processor, model


def transcribe_one(
    processor: Any, model: Any, args: argparse.Namespace, row: dict[str, Any]
) -> tuple[str, int, bool]:
    """Single documented unconditioned call. Returns (hypothesis, max_new_tokens, truncated)."""
    import torch

    max_new_tokens = compute_max_new_tokens(row["duration_s"])
    inputs = processor.apply_transcription_request(
        audio=row["path"],
        language=args.language,
    ).to(model.device, model.dtype)

    with torch.inference_mode():
        output_ids = model.generate(
            **inputs,
            max_new_tokens=max_new_tokens,
            do_sample=False,
        )
    generated_ids = output_ids[:, inputs["input_ids"].shape[1] :]
    hyp = processor.decode(generated_ids, return_format="transcription_only")[0]
    truncated = generated_ids.shape[1] >= max_new_tokens
    return hyp, max_new_tokens, truncated


# ==========================================================================
# Env record
# ==========================================================================
def build_env_record(args: argparse.Namespace, framework_version: str | None, max_new_tokens: int) -> dict[str, Any]:
    gpu_info = detect_gpu()
    ram_mb = detect_ram_mb()
    return {
        "framework": "transformers",
        "framework_version": framework_version,
        "model_revision": PINNED_REVISION,
        "dtype": args.dtype,
        "device": args.device,
        "attn_impl": args.attn_impl,
        "decode_mode": "greedy",
        "beam_width": 1,
        "temperature": None,
        "lang_token": args.language,
        "task_token": "transcribe",
        "punct_case_flag": "model_default",
        "max_input_s": None,  # no manual duration cap applied -- see README cap_risk
        "max_new_tokens": max_new_tokens,
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

    # One warm-up call, untimed, excluded from every clip's decode_s.
    if rows:
        try:
            transcribe_one(processor, model, args, rows[0])
        except Exception as exc:  # warm-up failure is non-fatal, just logged
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
                    hyp_text, _max_new_tokens, truncated = transcribe_one(processor, model, args, row)
                    decode_s = time.perf_counter() - t0
                    if not hyp_text or not hyp_text.strip():
                        row_status = "empty"
                    elif truncated:
                        row_status = "truncation"
                    else:
                        row_status = "ok"
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
                # NaN and generic crashes both fold into "crash" for
                # status.json.completion -- that enum has no separate NaN
                # value (see asr-bakeoff/CONTRACT.md).
                classified = "oom" if ("out of memory" in msg or "cuda oom" in msg) else "crash"
                bump_worst(classified)
                print(
                    f"[row {row['id']}] failed after {attempts} attempt(s): {last_exc}",
                    file=sys.stderr,
                )
                traceback.print_exc(file=sys.stderr)
                if classified == "oom":
                    stop_lane = True  # do not keep hammering an OOM'd device
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
            write_status()  # checkpoint after every clip

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
    parser.add_argument("--attn-impl", default="sdpa")
    parser.add_argument("--language", default="en", help="fixed explicitly per the decode contract")
    parser.add_argument("--seed", type=int, default=0)
    return parser


def _try_resolve_snapshot_dir_for_report(model_dir: Path) -> str | None:
    try:
        return str(resolve_snapshot_dir(model_dir))
    except SystemExit:
        return None
    except Exception:
        return None


def dry_run(args: argparse.Namespace, rows: list[dict[str, Any]]) -> int:
    max_new_tokens = compute_max_new_tokens(MAX_AUDIO_S_CONTRACT)
    resolved = {
        "model_key": MODEL_KEY,
        "repo_id": REPO_ID,
        "pinned_revision": PINNED_REVISION,
        "model_dir": str(args.model_dir),
        "model_dir_exists": args.model_dir.exists(),
        "resolved_snapshot_dir": _try_resolve_snapshot_dir_for_report(args.model_dir),
        "out_dir": str(args.out_dir),
        "manifest": str(args.manifest),
        "manifest_rows": len(rows),
        "device_requested": args.device,
        "dtype": args.dtype,
        "language": args.language,
        "attn_impl": args.attn_impl,
        "seed": args.seed,
        "max_new_tokens_at_contract_max_clip": max_new_tokens,
        "cap_risk": f"~{QWEN_LEGACY_SIBLING_CAP_S:g}s (unconfirmed for 1.7B; see README)",
    }
    preflight = preflight_vram_estimate(args.model_dir, args.dtype, MAX_AUDIO_S_CONTRACT, max_new_tokens)
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

    max_new_tokens = compute_max_new_tokens(MAX_AUDIO_S_CONTRACT)
    env_record = build_env_record(args, transformers.__version__, max_new_tokens)
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
