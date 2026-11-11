#!/usr/bin/env python3
"""Bakeoff inference adapter -- Higgs Audio v3 STT (bosonai, transformers-family
with a vendor-bundled standalone transcription helper).

Official inference reference (fetched 2026-07-17):
  https://huggingface.co/bosonai/higgs-audio-v3-stt
  AutoConfig/AutoModel/AutoTokenizer with trust_remote_code=True, PLUS the
  model repo's own bundled `transcribe.py` (its README explicitly says:
  "For the exact pipeline used in our evaluations (including the
  deterministic repetition/loop post-processing), use transcribe.py bundled
  in this repo: transcribe() / transcribe_batch()." That IS the official/
  recommended path for this model -- not the shorter inline snippet on the
  card, which the card itself says is superseded by transcribe.py for
  anything beyond a smoke test).

This adapter therefore does NOT hand-copy transcribe.py's ~300 lines of
prompt construction / collator setup / post-processing. It dynamically
imports the pinned revision's OWN transcribe.py directly from the
downloaded snapshot (`--model-dir`), guaranteeing byte-identical behavior
with the pinned revision and picking up any future revision bump
automatically. See README.md for the full source excerpt and the exact
files this requires the acquisition lane to have downloaded (the FULL repo
snapshot, not just *.safetensors).

Contract: ../../CONTRACT.md (SHARED I/O CONTRACT). This adapter is name-blind:
it only ever reads {id, path, duration_s, sha256} manifest rows. It never
sees ground truth, clip identity beyond the opaque id, source ordering, or
any vocabulary/context/bias payload, and it adds none of its own beyond a
fixed English-language directive appended to the vendor's own default
transcription prompt (see README.md "Language directive").

This file authors code only. It does NOT install packages, download
weights, or run inference by itself -- execution is gated on the frozen run
packet's approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) and on the
acquisition lane having populated --model-dir with the FULL repo snapshot.
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
MODEL_KEY = "higgs-audio-v3-stt"
REPO_ID = "bosonai/higgs-audio-v3-stt"
PINNED_REVISION = "2ffd1aa39f5a1266931e405cba12e404a9f994b2"
DEFAULT_MODEL_DIR = Path.cwd() / ".cache" / "asr-models" / "higgs-audio-v3-stt"
DEFAULT_OUT_DIR = Path.cwd() / "out" / MODEL_KEY

# Contract's declared maximum smoke-clip duration (librispeech_032_long120.wav).
MAX_AUDIO_S_CONTRACT = 110.75

# --------------------------------------------------------------------------
# Files transcribe.py needs alongside itself in --model-dir (all are
# siblings in the pinned revision, confirmed via the HF Hub API file listing
# fetched 2026-07-17 -- NOT part of the AutoConfig/AutoModel trust_remote_code
# auto_map, so a weights-only download would be missing them):
#   transcribe.py, higgs_audio_collator.py, modeling_higgs_audio_xcodec.py,
#   utils.py, common.py, custom_modules.py, attention.py,
#   cuda_graph_runner.py, ngram_loop_fix.py
# --------------------------------------------------------------------------
REQUIRED_VENDOR_FILES = (
    "transcribe.py",
    "higgs_audio_collator.py",
    "modeling_higgs_audio_xcodec.py",
    "utils.py",
    "configuration_higgs_audio.py",
    "modeling_higgs_audio.py",
)

# Vendor transcribe.py default (DEFAULT_PROMPT in transcribe.py): "Transcribe
# the speech. Output only the spoken words in lowercase with no punctuation."
# No language is named. Higgs supports many languages with auto-detection
# (README: sophisticated language detection); the decode contract requires
# English to be fixed explicitly wherever the model accepts a language
# instruction. Since transcribe.py's only language-selection surface is this
# free-text prompt (no formal `language=` kwarg), this is the mechanism --
# not vocabulary/context/decode-time bias. See README.md "Language directive".
TRANSCRIBE_PROMPT_EN = (
    "Transcribe the speech in English. Output only the spoken words in "
    "lowercase with no punctuation."
)

MAX_NEW_TOKENS_DEFAULT = 1024  # transcribe.py's own default

BYTES_PER_PARAM = {"bfloat16": 2, "float16": 2, "float32": 4, "int8": 1}

# Documented architecture (README.md "## Architecture"): Whisper-Large-v3
# encoder + Qwen3-1.7B decoder, 2.68B total params. Neither component's
# hidden_size/num_layers is exposed in this repo's own config.json summary
# via the HF API (checked 2026-07-17); these are the PUBLIC, well-documented
# configs for those two named upstream models, used as a placeholder
# geometry -- NOT re-verified against this repo's actual config.json, which
# load_config_geometry() reads and overrides the moment --model-dir is
# populated.
FALLBACK_DECODER_HIDDEN_SIZE = 2048  # public Qwen3-1.7B config
FALLBACK_DECODER_NUM_LAYERS = 28  # public Qwen3-1.7B config
FALLBACK_ENCODER_DMODEL = 1280  # public Whisper-large-v3 config
FALLBACK_ENCODER_LAYERS = 32  # public Whisper-large-v3 config
FALLBACK_PARAM_COUNT = 2_675_546_112  # HF API safetensors.parameters.BF16, verified 2026-07-17

# Vendor collator default (higgs_audio_collator.py::_create_collator,
# `chunk_size_seconds=getattr(config, "chunk_size_seconds", 30)`). This is
# the model's OWN documented long-form mechanism -- audio longer than this
# is split into sequential chunks and separately Whisper-encoded, with the
# resulting audio tokens duplicated/concatenated internally
# (_process_and_duplicate_audio_tokens). No adapter-side chunking is
# implemented; this happens transparently inside the vendor collator that
# transcribe.py itself constructs.
VENDOR_CHUNK_SIZE_S_DEFAULT = 30
VENDOR_ENCODER_PADDING_METHOD_DEFAULT = "max_length"

ACTIVATION_OVERHEAD_MULTIPLIER = 1.5  # workspace/attention-buffer fudge factor, matches sibling adapters


# ==========================================================================
# Manifest handling (identical shape/behavior to the sibling transformers-
# family adapters -- see ../../CONTRACT.md "Candidate-visible manifest").
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


def whisper_processor_cache_status() -> dict[str, Any]:
    """transcribe.py's collator needs `WhisperProcessor.from_pretrained(
    "openai/whisper-large-v3")` -- a small (~few MB) non-weight config/vocab
    fetch, not the 3B-parameter Whisper-large-v3 model itself, but it still
    requires either network access on first run or a pre-populated default
    huggingface_hub cache. This is informational only for --dry-run; the
    adapter does not attempt to work around it (that would deviate from
    calling the vendor's own collator verbatim)."""
    try:
        cache_root = Path.home() / ".cache" / "huggingface" / "hub" / "models--openai--whisper-large-v3"
        if cache_root.exists() and any(cache_root.rglob("preprocessor_config.json")):
            return {"cached_locally": True, "cache_root": str(cache_root)}
        return {
            "cached_locally": False,
            "cache_root": str(cache_root),
            "note": (
                "transcribe.py's collator will attempt a network fetch of "
                "openai/whisper-large-v3's processor config on first load "
                "unless this cache is pre-populated."
            ),
        }
    except Exception as exc:
        return {"cached_locally": None, "error": str(exc)}


# ==========================================================================
# VRAM preflight estimator
# ==========================================================================
def load_config_geometry(model_dir: Path) -> dict[str, Any]:
    """Prefer the real config.json under model_dir; fall back to the
    documented public Qwen3-1.7B / Whisper-large-v3 placeholder geometry."""
    cfg_path = model_dir / "config.json"
    if cfg_path.exists():
        try:
            cfg = json.loads(cfg_path.read_text(encoding="utf-8"))

            def find(key: str, node: Any) -> Any:
                if isinstance(node, dict):
                    if key in node:
                        return node[key]
                    for value in node.values():
                        found = find(key, value)
                        if found is not None:
                            return found
                return None

            return {
                "decoder_hidden_size": find("hidden_size", cfg) or FALLBACK_DECODER_HIDDEN_SIZE,
                "decoder_num_layers": (
                    find("num_hidden_layers", cfg) or find("num_layers", cfg) or FALLBACK_DECODER_NUM_LAYERS
                ),
                "encoder_dmodel": find("d_model", cfg) or FALLBACK_ENCODER_DMODEL,
                "encoder_layers": find("encoder_layers", cfg) or FALLBACK_ENCODER_LAYERS,
                "chunk_size_seconds": cfg.get("chunk_size_seconds", VENDOR_CHUNK_SIZE_S_DEFAULT),
                "source": "config.json",
            }
        except Exception:
            pass
    return {
        "decoder_hidden_size": FALLBACK_DECODER_HIDDEN_SIZE,
        "decoder_num_layers": FALLBACK_DECODER_NUM_LAYERS,
        "encoder_dmodel": FALLBACK_ENCODER_DMODEL,
        "encoder_layers": FALLBACK_ENCODER_LAYERS,
        "chunk_size_seconds": VENDOR_CHUNK_SIZE_S_DEFAULT,
        "source": "fallback_placeholder_public_qwen3_1.7b_whisper_large_v3_configs",
    }


def estimate_weight_bytes(model_dir: Path, dtype: str) -> tuple[int, str]:
    total = 0
    found = False
    if model_dir.exists():
        for pattern in ("*.safetensors", "*.bin", "*.pt"):
            for f in model_dir.rglob(pattern):
                total += f.stat().st_size
                found = True
    if found and total > 0:
        return total, "on_disk_weights"
    approx = int(FALLBACK_PARAM_COUNT * BYTES_PER_PARAM.get(dtype, 2))
    return approx, "hf_api_param_count_placeholder"


def preflight_vram_estimate(model_dir: Path, dtype: str, max_new_tokens: int) -> dict[str, Any]:
    weight_bytes, weight_source = estimate_weight_bytes(model_dir, dtype)
    geometry = load_config_geometry(model_dir)
    bytes_elem = BYTES_PER_PARAM.get(dtype, 2)

    # Long clips are internally chunked to chunk_size_seconds by the vendor
    # collator (see VENDOR_CHUNK_SIZE_S_DEFAULT docstring above), so the
    # per-forward encoder activation is sized for ONE chunk, not the full
    # clip -- but multiple chunks' audio tokens all land in the same decoder
    # sequence, so the decoder KV estimate scales with the full clip's
    # audio-token count, not one chunk's.
    chunk_s = geometry["chunk_size_seconds"]
    encoder_frames_per_chunk = chunk_s * 100  # WhisperFeatureExtractor hop=10ms -> 100 frames/sec
    encoder_activation_bytes = (
        encoder_frames_per_chunk * geometry["encoder_dmodel"] * geometry["encoder_layers"] * bytes_elem
    )

    num_chunks = max(1, -(-MAX_AUDIO_S_CONTRACT // chunk_s))  # ceil division, contract's max clip
    audio_tokens_estimate = num_chunks * (encoder_frames_per_chunk / 2)  # rough post-downsample estimate
    decoder_seq_len = audio_tokens_estimate + max_new_tokens
    decoder_kv_bytes = (
        2 * geometry["decoder_num_layers"] * geometry["decoder_hidden_size"] * bytes_elem * decoder_seq_len
    )

    total_bytes = (weight_bytes + encoder_activation_bytes + decoder_kv_bytes) * ACTIVATION_OVERHEAD_MULTIPLIER
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
        "encoder_activation_gb_per_chunk": round((encoder_activation_bytes * ACTIVATION_OVERHEAD_MULTIPLIER) / 1e9, 2),
        "decoder_kv_gb": round((decoder_kv_bytes * ACTIVATION_OVERHEAD_MULTIPLIER) / 1e9, 2),
        "assumptions": {
            **geometry,
            "num_chunks_at_contract_max_clip": num_chunks,
            "max_new_tokens": max_new_tokens,
            "activation_overhead_multiplier": ACTIVATION_OVERHEAD_MULTIPLIER,
            "note": (
                "decoder/encoder geometry falls back to the PUBLIC Qwen3-1.7B / "
                "Whisper-large-v3 configs (not this repo's own config.json) until "
                "--model-dir is populated with real weights -- treat as coarse. "
                "Packet's own pre-declared budget: '~5.4 GB BF16 weight floor... "
                "local smoke only if preflight fits' on an 8 GB Windows host."
            ),
        },
        "disposition_8gb": disposition,
    }


# ==========================================================================
# Inference (lazy heavy imports -- never imported for --dry-run)
# ==========================================================================
def resolve_snapshot_dir(model_dir: Path) -> Path:
    """If --model-dir looks like a huggingface_hub cache root (contains
    snapshots/<revision>/...), resolve to the exact pinned snapshot and
    refuse to silently substitute a different revision. Otherwise assume
    --model-dir already IS the pinned snapshot (the acquisition lane's
    documented layout for this bakeoff, confirmed for the ARK-ASR-0.6B
    lane's own download)."""
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


def check_vendor_files(snapshot_dir: Path) -> list[str]:
    """Files transcribe.py needs alongside itself (see REQUIRED_VENDOR_FILES
    docstring). Returns the list of missing filenames, empty if complete."""
    return [name for name in REQUIRED_VENDOR_FILES if not (snapshot_dir / name).exists()]


def import_vendor_transcribe(snapshot_dir: Path):
    """Dynamically import the pinned revision's own transcribe.py (and its
    sibling higgs_audio_collator.py etc.) from the downloaded snapshot,
    rather than hand-copying its ~300 lines here. Guarantees byte-identical
    behavior with the pinned revision; the README explicitly recommends this
    exact script for reproducing the model's reported numbers."""
    missing = check_vendor_files(snapshot_dir)
    if missing:
        raise FileNotFoundError(
            f"snapshot at {snapshot_dir} is missing vendor files required by "
            f"transcribe.py: {missing}. The acquisition lane must download the "
            "FULL repo snapshot (not just *.safetensors) -- see README.md."
        )
    snapshot_str = str(snapshot_dir)
    if snapshot_str not in sys.path:
        sys.path.insert(0, snapshot_str)
    import transcribe as vendor_transcribe  # type: ignore  # noqa: E402

    return vendor_transcribe


def load_model(args: argparse.Namespace, snapshot_dir: Path):
    import torch
    from transformers import AutoModel, AutoTokenizer

    dtype = getattr(torch, args.dtype)
    load_path = str(snapshot_dir)
    if args.device == "cpu":
        # README hardcodes device_map="cuda:0" -- CPU is not the documented
        # path for this model. Supported here for --dry-run completeness and
        # explicitly flagged as non-default; do not treat CPU results as
        # equivalent to the official path.
        model = AutoModel.from_pretrained(
            load_path,
            torch_dtype=dtype,
            trust_remote_code=True,
            attn_implementation=args.attn_impl,
        ).to("cpu")
    else:
        model = AutoModel.from_pretrained(
            load_path,
            torch_dtype=dtype,
            trust_remote_code=True,
            attn_implementation=args.attn_impl,
            device_map=args.cuda_device_map,
        )
    model.eval()
    tokenizer = AutoTokenizer.from_pretrained(load_path)
    return model, tokenizer


def load_audio_mono_float32(path: str) -> tuple[Any, int]:
    """Matches the model card's own example loader (`soundfile.read`, with a
    librosa resample only if the source sample rate isn't already 16 kHz --
    the run packet's corpus is already conditioned to 16 kHz mono, so this
    is expected to be a no-op in practice). Falls back to the stdlib `wave`
    module if soundfile is unavailable, for a 16-bit PCM WAV only."""
    import numpy as np

    try:
        import soundfile as sf

        audio_np, sr = sf.read(path, dtype="float32", always_2d=False)
        if getattr(audio_np, "ndim", 1) > 1:
            audio_np = audio_np.mean(axis=1)
        return np.asarray(audio_np, dtype=np.float32), int(sr)
    except ImportError:
        import wave

        with wave.open(path, "rb") as w:
            sr = w.getframerate()
            raw = w.readframes(w.getnframes())
            audio_np = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
            if w.getnchannels() > 1:
                audio_np = audio_np.reshape(-1, w.getnchannels()).mean(axis=1)
        return audio_np, int(sr)


def transcribe_one(
    vendor_transcribe: Any,
    model: Any,
    tokenizer: Any,
    args: argparse.Namespace,
    row: dict[str, Any],
) -> str:
    """Single call to the vendor's own official `transcribe()` (not
    `transcribe_batch()` -- batch size 1 keeps the per-clip checkpoint/retry
    contract simple and is the same public API entry point). Audio loading
    happens OUTSIDE the timed section; only the vendor `transcribe()` call
    itself is timed as `decode_s`, per the contract's "wall-clock decode
    time" for the scored portion."""
    audio_np, sr = load_audio_mono_float32(row["path"])
    hyp = vendor_transcribe.transcribe(
        model,
        tokenizer,
        audio_np,
        sample_rate=sr,
        user_prompt=TRANSCRIBE_PROMPT_EN,
        enable_thinking=True,
        max_new_tokens=args.max_new_tokens,
    )
    return hyp


# ==========================================================================
# Env record
# ==========================================================================
def build_env_record(args: argparse.Namespace, framework_version: str | None) -> dict[str, Any]:
    gpu_info = detect_gpu()
    ram_mb = detect_ram_mb()
    non_default: dict[str, Any] = {
        "language_directive": (
            f'prompt text "{TRANSCRIBE_PROMPT_EN}" (vendor transcribe.py has no '
            "formal language parameter; English fixed via the instruction "
            "string itself, per the decode contract -- not content/vocabulary "
            "bias; the vendor's own DEFAULT_PROMPT names no language)"
        ),
        "official_path": (
            "vendor-bundled transcribe.py (transcribe()), per this model's own "
            "README recommendation -- not the shorter inline card snippet"
        ),
        "post_processing": (
            "transcribe.py applies its own bundled _fix_repetitions "
            "(consecutive-word repetition cap) and fix_ngram_loops "
            "(phrase-level repetition-loop collapse) inside transcribe() -- "
            "this is the model author's own official/recommended output "
            "post-processing, not an adapter-added transform. No case/"
            "punctuation normalizer is applied inside transcribe() itself "
            "(the pip whisper_normalizer.english.EnglishTextNormalizer "
            "referenced in the module docstring is applied only in the "
            "vendor's separate leaderboard eval harness, not in the public "
            "transcribe()/transcribe_batch() API this adapter calls)."
        ),
        "whisper_processor_dependency": whisper_processor_cache_status(),
    }
    if args.device == "cpu":
        non_default["device"] = "cpu (README documents cuda:0 only; not the official path)"
    return {
        "framework": "transformers",
        "framework_version": framework_version,
        "model_revision": PINNED_REVISION,
        "dtype": args.dtype,
        "device": args.cuda_device_map if args.device == "cuda" else "cpu",
        "attn_impl": args.attn_impl,
        "decode_mode": "greedy",
        "beam_width": 1,
        "temperature": None,
        "lang_token": "en",
        "task_token": "transcribe",
        "punct_case_flag": "lowercase_no_punct_forced_by_prompt",
        "max_input_s": None,  # no adapter-side cap -- vendor collator chunks internally
        "max_new_tokens": args.max_new_tokens,
        "batch_size": 1,
        "vad": "none",
        "waveform_norm": "none",
        "chunk_size_s": VENDOR_CHUNK_SIZE_S_DEFAULT,
        "stride_s": 0,
        "overlap_s": 0,
        "reassembly": "vendor_collator_internal_audio_token_duplication (higgs_audio_collator.py::_process_and_duplicate_audio_tokens)",
        "timestamps": False,
        "prompt_context_bias": "none",
        "seed": args.seed,
        "non_default_params": non_default,
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
    vendor_transcribe: Any,
    model: Any,
    tokenizer: Any,
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
            transcribe_one(vendor_transcribe, model, tokenizer, args, rows[0])
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
                    hyp_text = transcribe_one(vendor_transcribe, model, tokenizer, args, row)
                    decode_s = time.perf_counter() - t0
                    row_status = "empty" if (not hyp_text or not hyp_text.strip()) else "ok"
                    # NOTE: this adapter cannot detect max_new_tokens-ceiling
                    # truncation through the public transcribe() API, which
                    # returns only the decoded string -- see README.md
                    # "Truncation detection is not available via the public
                    # API". status is therefore only ever ok/empty/unsupported
                    # for this lane, never "truncation".
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
    parser.add_argument(
        "--cuda-device-map",
        default="cuda:0",
        help="device_map passed to from_pretrained when --device cuda (README hardcodes 'cuda:0')",
    )
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    parser.add_argument("--dtype", choices=sorted(BYTES_PER_PARAM), default="bfloat16")
    parser.add_argument("--attn-impl", default="eager", help="README's own example uses 'eager'")
    parser.add_argument("--max-new-tokens", type=int, default=MAX_NEW_TOKENS_DEFAULT)
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
    snapshot_dir_str = _try_resolve_snapshot_dir_for_report(args.model_dir)
    missing_vendor_files = (
        check_vendor_files(Path(snapshot_dir_str)) if snapshot_dir_str else list(REQUIRED_VENDOR_FILES)
    )
    resolved = {
        "model_key": MODEL_KEY,
        "repo_id": REPO_ID,
        "pinned_revision": PINNED_REVISION,
        "model_dir": str(args.model_dir),
        "model_dir_exists": args.model_dir.exists(),
        "resolved_snapshot_dir": snapshot_dir_str,
        "missing_vendor_files": missing_vendor_files,
        "out_dir": str(args.out_dir),
        "manifest": str(args.manifest),
        "manifest_rows": len(rows),
        "device_requested": args.device,
        "cuda_device_map": args.cuda_device_map,
        "dtype": args.dtype,
        "attn_impl": args.attn_impl,
        "max_new_tokens": args.max_new_tokens,
        "language_directive": TRANSCRIBE_PROMPT_EN,
        "seed": args.seed,
        "whisper_processor_dependency": whisper_processor_cache_status(),
        "long_form_path": (
            f"vendor collator internal chunking, chunk_size_seconds="
            f"{VENDOR_CHUNK_SIZE_S_DEFAULT} (documented, not adapter-invented) -- "
            "see higgs_audio_collator.py::_process_and_duplicate_audio_tokens"
        ),
        "truncation_detection_caveat": (
            "the public transcribe()/transcribe_batch() API returns only the "
            "decoded string, not raw token counts -- this adapter cannot "
            "distinguish a max_new_tokens-ceiling truncation from a natural "
            "end-of-generation. status is therefore never 'truncation' for "
            "this lane; see README.md."
        ),
    }
    preflight = preflight_vram_estimate(args.model_dir, args.dtype, args.max_new_tokens)
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

    snapshot_dir = resolve_snapshot_dir(args.model_dir)
    missing_vendor_files = check_vendor_files(snapshot_dir)
    if missing_vendor_files:
        print(
            f"snapshot at {snapshot_dir} is missing vendor files required by "
            f"transcribe.py: {missing_vendor_files}. The acquisition lane must "
            "download the FULL repo snapshot, not just *.safetensors. See README.md.",
            file=sys.stderr,
        )
        return 3

    args.out_dir.mkdir(parents=True, exist_ok=True)
    hyp_path = args.out_dir / "hypotheses.jsonl"
    status_path = args.out_dir / "status.json"
    env_path = args.out_dir / "env.json"
    if hyp_path.exists():
        hyp_path.unlink()

    vendor_transcribe = import_vendor_transcribe(snapshot_dir)

    t_load0 = time.perf_counter()
    model, tokenizer = load_model(args, snapshot_dir)
    cold_load_s = time.perf_counter() - t_load0

    import transformers

    env_record = build_env_record(args, transformers.__version__)
    env_path.write_text(json.dumps(env_record, indent=2), encoding="utf-8")

    rows_done, worst, retries_used = process_rows(
        vendor_transcribe, model, tokenizer, args, selected, hyp_path, status_path, cold_load_s
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
