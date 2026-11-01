#!/usr/bin/env python3
"""Bakeoff inference adapter — nvidia/canary-1b-flash (NeMo-native).

Official inference reference (fetched 2026-07-18):
  https://huggingface.co/nvidia/canary-1b-flash
  "Loading the Model" + default-English-ASR + "Longform inference" sections.

Contract: ../_contract.md (SHARED I/O CONTRACT). This adapter is name-blind:
it only ever reads {id, path, duration_s, sha256} manifest rows. It never
sees ground truth, clip identity beyond the opaque id, source ordering, or
any vocabulary/context/bias payload, and it adds none of its own -- only the
model's documented unconditioned ASR call is used.

Why NeMo, not transformers: canary-1b-flash's HF repo ships a transformers-
shaped config.json (architectures: ["FastConformerModel"]) alongside its
.nemo file, but that architecture class is NOT registered in the installed
transformers 5.14.1 (verified: `hasattr(transformers, "FastConformerModel")
== False`) and the config's `nemo_decoder_type: "none"` indicates the
transformers export is an encoder-only interop artifact, not a full
ASR pipeline. The model card's only documented working inference path is
NeMo's `EncDecMultiTaskModel`. NeMo 2.7.3 (pinned, isolated venv
`.venv-infer-nemo`) imports and reports CUDA available on this host --
no Windows-specific NeMo failure was hit for this checkpoint.

Roster extension authorization: run-contract.json "roster_extensions"
(2026-07-18, Adrian explicit chat approval, recorded per BAKEOFF_RUN_GATE.md
local lane -- isolation proof + Adrian's explicit approval, both satisfied).

## 40-second design cap and the documented longform strategy

The model card states Canary models are "designed to handle input audio
smaller than 40 seconds" and documents a longform script
(`speech_to_text_aed_chunked_infer.py`, chunk_len_in_secs=40.0) for longer
audio: "chunks long audios into non-overlapping segments of
chunk_len_in_secs seconds and performs inference on each segment
individually" (source inspected 2026-07-18), with transcript stitching
being simple ordered concatenation of segment transcriptions -- no explicit
delimiter or dedup logic in that script.

This adapter reimplements exactly that documented procedure (non-overlapping
40.0s segments, per-segment `.transcribe()`, ordered concatenation) rather
than the script's internal `FrameBatchMultiTaskAED` buffered-feature class,
because (a) the per-chunk unit of work is still the model's own documented
public `.transcribe()` call -- nothing about the ASR call itself is invented
-- and (b) the buffered-feature class exists for streaming efficiency, not
for a different chunking/stitching contract. `env.json` records
`chunk_size_s=40.0`/`stride_s=40.0`/`overlap_s=0.0`/`reassembly="concat"`
for any row where this path fires, so it is never silently conflated with
the single-shot <=40s path in `decode_mode`/`non_default_params`.
"""
from __future__ import annotations

import argparse
import json
import math
import platform
import sys
import tempfile
import time
import traceback
from pathlib import Path
from typing import Any

# --------------------------------------------------------------------------
# Pinned identity (never silently substituted).
# --------------------------------------------------------------------------
MODEL_KEY = "canary-1b-flash"
REPO_ID = "nvidia/canary-1b-flash"
PINNED_REVISION = "2b6e4d2dacb11cc1b1724de31bb48fe68c26c12e"
DEFAULT_MODEL_DIR = Path(__file__).resolve().parents[5] / ".cache" / "asr-models" / "canary-1b-flash"
DEFAULT_OUT_DIR = Path(__file__).resolve().parents[2] / "out" / MODEL_KEY

# Model card's documented design cap and longform chunk size (same value).
LONGFORM_CAP_S = 40.0


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


def preflight_vram_estimate(model_dir: Path) -> dict[str, Any]:
    total = 0
    found = False
    if model_dir.exists():
        for f in model_dir.rglob("*.safetensors"):
            total += f.stat().st_size
            found = True
        if not found:
            for f in model_dir.rglob("*.nemo"):
                total += f.stat().st_size
                found = True
    weight_source = "on_disk_weights" if found else "not_acquired"
    total_gb = (total * 1.6) / 1e9  # activation/workspace fudge factor (encoder-decoder, beam=1)
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
        "note": "883M-param FastConformer-Transformer AED; beam_size=1 per model card's own decode-config example.",
    }


# ==========================================================================
# Model loading + inference (lazy heavy imports -- never imported for --dry-run)
# ==========================================================================
def resolve_nemo_file(model_dir: Path) -> Path:
    candidates = list(model_dir.glob("*.nemo"))
    if not candidates:
        raise SystemExit(f"no .nemo file found under {model_dir}")
    if len(candidates) > 1:
        print(
            f"multiple .nemo files under {model_dir}: {[c.name for c in candidates]}; "
            "refusing to silently pick one",
            file=sys.stderr,
        )
        raise SystemExit(4)
    return candidates[0]


def load_model(args: argparse.Namespace):
    from nemo.collections.asr.models import EncDecMultiTaskModel

    nemo_file = resolve_nemo_file(args.model_dir)
    map_location = "cuda" if args.device == "cuda" else "cpu"
    model = EncDecMultiTaskModel.restore_from(str(nemo_file), map_location=map_location)
    decode_cfg = model.cfg.decoding
    decode_cfg.beam.beam_size = args.beam_size
    model.change_decoding_strategy(decode_cfg)
    model.eval()
    return model


def _chunk_bounds(duration_s: float, chunk_s: float) -> list[tuple[float, float]]:
    n_chunks = max(1, math.ceil(duration_s / chunk_s))
    return [(i * chunk_s, min((i + 1) * chunk_s, duration_s)) for i in range(n_chunks)]


def transcribe_one(model: Any, args: argparse.Namespace, row: dict[str, Any], tmp_dir: Path) -> tuple[str, bool]:
    """Returns (hypothesis, used_longform_chunking)."""
    duration_s = row["duration_s"]

    if duration_s <= LONGFORM_CAP_S:
        out = model.transcribe([row["path"]], batch_size=1, pnc="yes", verbose=False)
        text = out[0].text if hasattr(out[0], "text") else out[0]
        return text, False

    # Documented longform path: non-overlapping LONGFORM_CAP_S-second segments,
    # per-segment transcribe(), ordered concatenation (see module docstring).
    import soundfile as sf

    audio, sr = sf.read(row["path"], dtype="float32", always_2d=False)
    bounds = _chunk_bounds(duration_s, LONGFORM_CAP_S)
    parts: list[str] = []
    chunk_paths: list[Path] = []
    try:
        for idx, (t0, t1) in enumerate(bounds):
            s0, s1 = int(t0 * sr), min(int(t1 * sr), len(audio))
            seg = audio[s0:s1]
            chunk_path = tmp_dir / f"{row['id']}_chunk{idx}.wav"
            sf.write(str(chunk_path), seg, sr)
            chunk_paths.append(chunk_path)
            out = model.transcribe([str(chunk_path)], batch_size=1, pnc="yes", verbose=False)
            text = out[0].text if hasattr(out[0], "text") else out[0]
            parts.append(text or "")
    finally:
        for p in chunk_paths:
            p.unlink(missing_ok=True)

    return " ".join(parts).strip(), True


# ==========================================================================
# Env record
# ==========================================================================
def build_env_record(args: argparse.Namespace, nemo_version: str | None, longform_used_any: bool) -> dict[str, Any]:
    gpu_info = detect_gpu()
    ram_mb = detect_ram_mb()
    return {
        "framework": "nemo",
        "framework_version": nemo_version,
        "model_revision": PINNED_REVISION,
        "dtype": "float32",  # NeMo restore_from default; no explicit half-precision cast applied
        "device": args.device,
        "attn_impl": "model_default",
        "decode_mode": "aed_beam_offline" + ("+longform_chunked_40s_where_needed" if longform_used_any else ""),
        "beam_width": args.beam_size,
        "temperature": None,
        "lang_token": "en",  # default behavior when passing plain file paths (canary assumes English ASR)
        "task_token": "asr",
        "punct_case_flag": "pnc=yes",
        "max_input_s": None,
        "max_new_tokens": None,
        "batch_size": 1,
        "vad": "none",
        "waveform_norm": "none",
        "chunk_size_s": LONGFORM_CAP_S if longform_used_any else None,
        "stride_s": LONGFORM_CAP_S if longform_used_any else None,
        "overlap_s": 0.0 if longform_used_any else None,
        "reassembly": "concat" if longform_used_any else None,
        "timestamps": False,
        "prompt_context_bias": "none",
        "seed": args.seed,
        "non_default_params": {
            "longform_cap_s": LONGFORM_CAP_S,
            "longform_strategy": "model card documented speech_to_text_aed_chunked_infer.py behavior (non-overlapping chunk_len_in_secs=40.0, ordered concatenation), reimplemented via per-chunk .transcribe() -- see adapter.py docstring",
        },
        "gpu": gpu_info.get("gpu"),
        "vram_mb": gpu_info.get("vram_mb"),
        "ram": ram_mb,
        "os": platform.platform(),
        "driver": gpu_info.get("driver"),
        "host_class": host_class(gpu_info, ram_mb),
    }


# ==========================================================================
# Row processing
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
    model: Any,
    args: argparse.Namespace,
    rows: list[dict[str, Any]],
    hyp_path: Path,
    status_path: Path,
    cold_load_s: float,
    tmp_dir: Path,
) -> tuple[int, str, int, bool]:
    worst = "ok"
    retries_used = 0
    rows_done = 0
    longform_used_any = False

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
            transcribe_one(model, args, rows[0], tmp_dir)
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
            row_longform = False

            while attempts < 2:
                attempts += 1
                try:
                    t0 = time.perf_counter()
                    hyp_text, row_longform = transcribe_one(model, args, row, tmp_dir)
                    decode_s = time.perf_counter() - t0
                    row_status = "empty" if not hyp_text or not hyp_text.strip() else "ok"
                    last_exc = None
                    break
                except Exception as exc:  # noqa: BLE001
                    last_exc = exc
                    continue

            longform_used_any = longform_used_any or row_longform
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

    return rows_done, worst, retries_used, longform_used_any


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
    parser.add_argument("--beam-size", type=int, default=1, help="matches model card's own decode-config example")
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
        "beam_size": args.beam_size,
        "seed": args.seed,
        "longform_cap_s": LONGFORM_CAP_S,
        "rows_over_cap": sum(1 for r in rows if r["duration_s"] > LONGFORM_CAP_S),
    }
    preflight = preflight_vram_estimate(args.model_dir)
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
    model = load_model(args)
    cold_load_s = time.perf_counter() - t_load0

    import nemo

    with tempfile.TemporaryDirectory(prefix="canary_chunks_") as tmp_dir_str:
        tmp_dir = Path(tmp_dir_str)
        rows_done, worst, retries_used, longform_used_any = process_rows(
            model, args, selected, hyp_path, status_path, cold_load_s, tmp_dir
        )

    env_record = build_env_record(args, getattr(nemo, "__version__", None), longform_used_any)
    env_path.write_text(json.dumps(env_record, indent=2), encoding="utf-8")

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

    print(f"done: {rows_done}/{len(selected)} rows, completion={worst}, longform_used_any={longform_used_any}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
