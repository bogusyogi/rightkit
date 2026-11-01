#!/usr/bin/env python3
"""Shared loader/runner for the AutoArk-AI ARK-ASR family (0.6B and 3B).

Both checkpoints ship the identical custom `arkasr` remote-code loader
(`processing_arkasr.ArkasrProcessor` + `modeling_arkasr.ArkasrForConditionalGeneration`,
via `AutoProcessor`/`AutoTokenizer`/`AutoModelForCausalLM` + `trust_remote_code=True`)
per both model cards -- confirmed by diffing the sibling-file manifests of
https://huggingface.co/AutoArk-AI/ARK-ASR-0.6B and
https://huggingface.co/AutoArk-AI/ARK-ASR-3B (both list the same
`processing_arkasr.py` / `modeling_arkasr.py` / `modeling_audio.py` /
`configuration_arkasr.py` set, fetched 2026-07-17) and by the 3B card's own
text ("The model should be loaded with `trust_remote_code=True`" using
"identical APIs" to the 0.6B sibling). `ark-asr-0.6b/adapter.py` and
`ark-asr-3b/adapter.py` are both thin CLI shims over this module -- see the
task instructions ("factor shared code into ark_common.py in the 0.6B dir
and import it from 3B").

Ground-truthed against the ACTUALLY DOWNLOADED 0.6B snapshot at
`D:\\Claude\\heardright\\.cache\\asr-models\\ark-asr-0.6b\\` (acquisition
lane completed this download 2026-07-17T15:50Z per
`../acquire/acquisition-ledger.json`) -- `config.json`,
`processing_arkasr.py`, `modeling_arkasr.py`, `preprocessor_config.json`,
`processor_config.json`, `generation_config.json`, `README.md` were all read
directly, not paraphrased from a fetch summary. The 3B snapshot was not
locally available at authoring time; its geometry constants below are
placeholders (see `FALLBACK_*_3B`) until `load_config_geometry()` can read
the real `config.json`, which it does automatically once the acquisition
lane populates `--model-dir` for 3B.

Contract: ../_contract.md (SHARED I/O CONTRACT). Every adapter using this
module is name-blind: it only ever reads {id, path, duration_s, sha256}
manifest rows. It never sees ground truth, clip identity beyond the opaque
id, source ordering, or any vocabulary/context/bias payload, and it adds
none of its own -- only the model's documented unconditioned ASR call is
used (`apply_chat_template` + a fixed "Please transcribe this audio in
English." instruction, English fixed per the decode contract since ARK has
no formal language-token parameter -- see README.md "language directive"
section for why this is not context/bias).

This file authors code only. It does NOT install packages, download
weights, or run inference by itself -- execution is gated on the frozen run
packet's approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) and on the
acquisition lane having populated --model-dir.
"""
from __future__ import annotations

import argparse
import dataclasses
import json
import platform
import sys
import time
import traceback
from pathlib import Path
from typing import Any

# ==========================================================================
# Architectural hard cap -- NOT a tunable default.
#
# config.json (whisper_config): "max_source_positions": 1500. preprocessor_
# config.json: "n_samples": 480000, "nb_max_frames": 3000. 480000 samples /
# 16000 Hz = 30.0 s; 3000 raw mel frames -> stride-2 conv -> 1500 encoder
# positions. The encoder's absolute position-embedding table has exactly
# 1500 rows.
#
# processing_arkasr.py's apply_chat_template passes `audio_max_length`
# straight into `self.feature_extractor(..., padding=audio_padding,
# max_length=audio_max_length, ...)`. `WhisperFeatureExtractor.__call__`
# defaults `truncation=True`, so any raw audio longer than
# `audio_max_length` samples is SILENTLY TRUNCATED at the feature-extraction
# step -- no exception. `_calculate_audio_token_counts_per_sample` mirrors
# the same cap when sizing the `<|audio|>` placeholder run, so the prompt
# and the truncated features stay consistent (no crash there either).
#
# Raising `audio_max_length` PAST 30 s does not "unlock" longer audio: the
# Whisper encoder's `embed_pos = self.embed_positions.weight[:inputs_embeds
# .shape[1]]` slice only has 1500 rows to give, so on a >1500-frame input
# the addition of `embed_pos` (1500 rows) to `inputs_embeds` (>1500 rows)
# shape-mismatches and raises a RuntimeError. So: at or under 30 s ->
# silent truncation to whatever cap is configured (default 30 s = no
# truncation for that class of clip); over 30 s with an enlarged cap ->
# crash, not extended support. `--max-audio-s` exists only for controlled
# experiments at this boundary, not as a fix for long-form audio -- there is
# no documented long-form/chunking path for this model family.
# ==========================================================================
MAX_AUDIO_S_ARCHITECTURAL = 30.0

# Contract's declared maximum smoke-clip duration (librispeech_032_long120.wav).
# Used only to describe risk in --dry-run output; ARK cannot actually consume
# a clip this long in one call (see MAX_AUDIO_S_ARCHITECTURAL above).
MAX_AUDIO_S_CONTRACT = 110.75

# Official example's fixed generation budget (README.md "## Inference").
# Not scaled per-clip like the Qwen3-ASR sibling adapter -- ARK's audio input
# is hard-capped at 30 s regardless of the source clip's real duration, so a
# fixed post-truncation token budget is what the documented path actually
# uses.
MAX_NEW_TOKENS_DEFAULT = 256

# --------------------------------------------------------------------------
# Long-form windowed decode (added 2026-07-18, coordinator-directed follow-up
# to the frozen Pico-224 run). ARK's Whisper-style encoder has a hard
# architectural cap at MAX_AUDIO_S_ARCHITECTURAL (~30 s / 1500 encoder
# positions, see above) -- audio longer than that is either:
#   (a) `--long-form truncate` -- the RAW/comparator mode already used for
#       the frozen Pico-224 run: one single-shot call, silent truncation at
#       `--max-audio-s`, reported as `status=truncation`. This is the mode
#       that produced ARK-0.6B's 29.59% corpus WER (truncation damage on the
#       77 s/110.75 s clips), NOT an accuracy measurement of the model.
#   (b) `--long-form window` (default for this lane going forward) -- split
#       into <=WINDOW_S_DEFAULT-second windows with WINDOW_OVERLAP_S_DEFAULT
#       seconds of overlap, decode each window independently via the model's
#       normal documented call (using the processor's native
#       `begin_time`/`end_time` audio-slicing kwargs -- see
#       processing_arkasr.py's `_build_prompts_and_audio`, which already
#       supports per-part time-slicing for `"path"`-type audio parts), then
#       stitch the per-window transcripts with a longest-suffix/prefix
#       text-overlap dedup (`dedupe_stitch`, word-level, case/punct-insensitive
#       comparison). This is a documented PRODUCT-EVAL chunking variant, not
#       a frozen raw comparator -- it must never silently overwrite or be
#       confused with the truncate-mode Pico-224 result above.
# --------------------------------------------------------------------------
WINDOW_S_DEFAULT = 28.0  # stays safely under the ~30.0s / 1500-position architectural cap
WINDOW_OVERLAP_S_DEFAULT = 2.0

BYTES_PER_PARAM = {"bfloat16": 2, "float16": 2, "float32": 4, "int8": 1}

# Real, config.json-verified geometry for the 0.6B checkpoint (decoder side):
# hidden_size=896, num_hidden_layers=24 (Qwen2-0.5B-class decoder). Whisper
# encoder side: d_model=1280, encoder_layers=32 (Whisper-large-v3-class
# encoder -- this, not the 0.6B decoder, dominates the "0.6B" model's actual
# ~1.3B total parameter count per the HF API's
# `safetensors.parameters.BF16=1299463168`).
FALLBACK_HIDDEN_SIZE_0_6B = 896
FALLBACK_NUM_LAYERS_0_6B = 24
FALLBACK_WHISPER_DMODEL_0_6B = 1280
FALLBACK_WHISPER_ENCODER_LAYERS_0_6B = 32
FALLBACK_PARAM_COUNT_0_6B = 1_299_463_168  # HF API safetensors.parameters.BF16, verified 2026-07-17

# 3B checkpoint: NOT locally downloaded at authoring time. Placeholder
# geometry only, documented-uncertain -- load_config_geometry() overrides
# these the moment a real config.json exists under --model-dir.
FALLBACK_HIDDEN_SIZE_3B = 2048  # Qwen2-3B-class decoder, unverified guess
FALLBACK_NUM_LAYERS_3B = 36  # Qwen2-3B-class decoder, unverified guess
FALLBACK_WHISPER_DMODEL_3B = 1280  # same Whisper-large-v3-class encoder assumed, unverified
FALLBACK_WHISPER_ENCODER_LAYERS_3B = 32  # unverified
FALLBACK_PARAM_COUNT_3B = 4_063_438_848  # HF API safetensors.parameters.BF16, verified 2026-07-17

ACTIVATION_OVERHEAD_MULTIPLIER = 1.5  # workspace/attention-buffer fudge factor, matches sibling adapters


@dataclasses.dataclass(frozen=True)
class ArkModelSpec:
    model_key: str
    repo_id: str
    pinned_revision: str
    default_model_dir: Path
    default_dtype_cuda: str  # official per-model recommendation: "float16" (0.6B) / "bfloat16" (3B)
    hidden_size_fallback: int
    num_layers_fallback: int
    whisper_dmodel_fallback: int
    whisper_encoder_layers_fallback: int
    param_count_fallback: int


# ==========================================================================
# Manifest handling (identical shape/behavior to the sibling transformers-
# family adapters -- see ../_contract.md "Candidate-visible manifest").
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
def load_config_geometry(model_dir: Path, spec: ArkModelSpec) -> dict[str, Any]:
    """Prefer the real config.json under model_dir; fall back to the
    per-model documented placeholder geometry when weights are not yet
    acquired (or, for 3B, always until acquired)."""
    cfg_path = model_dir / "config.json"
    if cfg_path.exists():
        try:
            cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
            whisper_cfg = cfg.get("whisper_config", {}) or {}
            return {
                "hidden_size": cfg.get("hidden_size", spec.hidden_size_fallback),
                "num_layers": cfg.get("num_hidden_layers", spec.num_layers_fallback),
                "whisper_dmodel": whisper_cfg.get("d_model", spec.whisper_dmodel_fallback),
                "whisper_encoder_layers": whisper_cfg.get(
                    "encoder_layers", spec.whisper_encoder_layers_fallback
                ),
                "max_source_positions": whisper_cfg.get("max_source_positions", 1500),
                "source": "config.json",
            }
        except Exception:
            pass
    return {
        "hidden_size": spec.hidden_size_fallback,
        "num_layers": spec.num_layers_fallback,
        "whisper_dmodel": spec.whisper_dmodel_fallback,
        "whisper_encoder_layers": spec.whisper_encoder_layers_fallback,
        "max_source_positions": 1500,
        "source": "fallback_placeholder",
    }


def estimate_weight_bytes(model_dir: Path, dtype: str, spec: ArkModelSpec) -> tuple[int, str]:
    """Prefer real on-disk weight bytes; fall back to the HF-API-verified
    param count * bytes-per-param when weights are not present.

    On-disk safetensors are the checkpoint's shipped dtype (bf16 for both ARK
    checkpoints) regardless of the *runtime* ``--dtype`` requested. For
    ``dtype == "int8"`` (bitsandbytes weight-only quantization, added
    2026-07-18) that on-disk bf16 size overstates the actual runtime weight
    footprint, so it is scaled by the int8/bf16 byte-per-param ratio (1/2) --
    still an approximation (bnb keeps a handful of layers, e.g. embeddings,
    at higher precision), flagged via the returned source string rather than
    reported as a measured int8 size.
    """
    total = 0
    found = False
    if model_dir.exists():
        for pattern in ("*.safetensors", "*.bin", "*.pt"):
            for f in model_dir.rglob(pattern):
                total += f.stat().st_size
                found = True
    if found and total > 0:
        if dtype == "int8":
            scaled = int(total * (BYTES_PER_PARAM["int8"] / BYTES_PER_PARAM["bfloat16"]))
            return scaled, "on_disk_bf16_weights_scaled_to_int8_approx"
        return total, "on_disk_weights"
    approx = int(spec.param_count_fallback * BYTES_PER_PARAM.get(dtype, 2))
    return approx, "hf_api_param_count_placeholder"


def preflight_vram_estimate(
    model_dir: Path, dtype: str, spec: ArkModelSpec, max_new_tokens: int
) -> dict[str, Any]:
    weight_bytes, weight_source = estimate_weight_bytes(model_dir, dtype, spec)
    geometry = load_config_geometry(model_dir, spec)
    bytes_elem = BYTES_PER_PARAM.get(dtype, 2)

    # Audio input is architecturally capped at MAX_AUDIO_S_ARCHITECTURAL (see
    # the module docstring) -- the encoder never processes more than 1500
    # frames regardless of the source clip's real duration, so the
    # activation estimate is sized for that ceiling, not the contract's
    # 110.75 s clip.
    max_source_positions = geometry["max_source_positions"]
    encoder_activation_bytes = (
        max_source_positions
        * geometry["whisper_dmodel"]
        * geometry["whisper_encoder_layers"]
        * bytes_elem
    )
    decoder_seq_len = max_source_positions // 4 + max_new_tokens  # /4 == merge_factor (audio tokens after MLP adapter)
    decoder_kv_bytes = 2 * geometry["num_layers"] * geometry["hidden_size"] * bytes_elem * decoder_seq_len

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
        "encoder_activation_gb": round((encoder_activation_bytes * ACTIVATION_OVERHEAD_MULTIPLIER) / 1e9, 2),
        "decoder_kv_gb": round((decoder_kv_bytes * ACTIVATION_OVERHEAD_MULTIPLIER) / 1e9, 2),
        "assumptions": {
            **geometry,
            "max_new_tokens": max_new_tokens,
            "activation_overhead_multiplier": ACTIVATION_OVERHEAD_MULTIPLIER,
            "note": (
                "geometry comes from config.json when --model-dir is populated "
                "(true for 0.6B, confirmed 2026-07-17: hidden_size=896, "
                "num_hidden_layers=24, whisper d_model=1280, encoder_layers=32); "
                "otherwise it is a documented-uncertain placeholder (true for 3B "
                "until its weights are acquired) and the estimate should be "
                "treated as coarse."
            ),
        },
        "disposition_8gb": disposition,
    }


# ==========================================================================
# Inference (lazy heavy imports -- never imported for --dry-run)
# ==========================================================================
def resolve_snapshot_dir(model_dir: Path, spec: ArkModelSpec) -> Path:
    """If --model-dir looks like a huggingface_hub cache root (contains
    snapshots/<revision>/...), resolve to the exact pinned snapshot and
    refuse to silently substitute a different revision. Otherwise assume
    --model-dir already IS the pinned snapshot (the acquisition lane's
    documented layout, confirmed for 0.6B: a flat directory of config.json
    + *.py + *.safetensors, not a hub cache tree)."""
    snapshots_dir = model_dir / "snapshots"
    if snapshots_dir.exists():
        pinned = snapshots_dir / spec.pinned_revision
        if pinned.exists():
            return pinned
        available = [p.name for p in snapshots_dir.iterdir() if p.is_dir()]
        print(
            f"pinned revision {spec.pinned_revision} not found under {snapshots_dir}; "
            f"available snapshots: {available}. Refusing to silently substitute "
            "a sibling checkpoint.",
            file=sys.stderr,
        )
        raise SystemExit(4)
    return model_dir


def build_bad_words_ids(tokenizer) -> list[list[int]]:
    """Verbatim from the ARK-ASR-0.6B/3B model card README ('## Inference').
    Suppresses every special/added token except EOS from generation, so the
    model cannot emit stray control tokens into the transcript."""
    eos_ids = tokenizer.eos_token_id
    keep_ids = {eos_ids} if isinstance(eos_ids, int) else set(eos_ids or [])
    bad_ids = set(tokenizer.all_special_ids) - keep_ids
    bad_ids.update(
        token_id
        for token, token_id in tokenizer.get_added_vocab().items()
        if token.startswith("<") and token.endswith(">") and token_id not in keep_ids
    )
    return [[token_id] for token_id in sorted(bad_ids)]


def load_model(spec: ArkModelSpec, args: argparse.Namespace):
    import torch
    from transformers import AutoModelForCausalLM, AutoProcessor, AutoTokenizer

    snapshot_dir = resolve_snapshot_dir(args.model_dir, spec)
    load_path = str(snapshot_dir)
    processor = AutoProcessor.from_pretrained(load_path, trust_remote_code=True)
    tokenizer = AutoTokenizer.from_pretrained(load_path, trust_remote_code=True)

    if args.dtype == "int8":
        # bitsandbytes 8-bit weight-only quantization (added 2026-07-18 for
        # the ARK-ASR-3B 8 GB local-eval lane -- bf16 weights (~7.6 GB on
        # disk) exceed the 8 GB card; int8 halves the weight footprint.
        # This is a clearly-labelled quantized variant (env.json dtype=
        # "int8"), not a substitute for the bf16 reference result -- no
        # WER/quality claim is made about int8 vs bf16 by this adapter.
        from transformers import BitsAndBytesConfig

        quant_config = BitsAndBytesConfig(load_in_8bit=True)
        model = AutoModelForCausalLM.from_pretrained(
            load_path,
            trust_remote_code=True,
            quantization_config=quant_config,
            device_map="auto",
            attn_implementation=args.attn_impl,
        )
    else:
        dtype = getattr(torch, args.dtype)
        model = AutoModelForCausalLM.from_pretrained(
            load_path,
            trust_remote_code=True,
            torch_dtype=dtype,
            attn_implementation=args.attn_impl,
        ).to(args.device)
    model.eval()
    return processor, tokenizer, model


# Official example prompt is "Please transcribe this audio." with no language
# designation (ARK is a 19-language multilingual model with no formal
# language-token parameter in its chat-template API -- unlike Whisper's
# `language=` kwarg). The decode contract requires English to be fixed
# explicitly wherever the model "accepts/requires a language parameter";
# since ARK's only language-selection surface is this natural-language
# instruction string, appending "in English" is the mechanism, not
# vocabulary/context bias -- it names the task/language exactly as
# `language="en"` would on a model with a formal parameter, and nothing
# clip-specific is added. See README.md "Language directive" for the full
# reasoning.
TRANSCRIBE_PROMPT_EN = "Please transcribe this audio in English."


def transcribe_one(
    processor: Any,
    tokenizer: Any,
    model: Any,
    spec: ArkModelSpec,
    args: argparse.Namespace,
    row: dict[str, Any],
    begin_time: float | None = None,
    end_time: float | None = None,
    audio_max_length_s: float | None = None,
) -> tuple[str, bool]:
    """Single documented unconditioned call, verbatim structure from the
    model card's '## Inference' example. Returns (hypothesis, truncated).

    ``begin_time``/``end_time`` (added 2026-07-18) are optional windowed-decode
    slice bounds in seconds, passed straight through to the processor's own
    ``"path"``-type audio-part time-slicing (processing_arkasr.py's
    ``_build_prompts_and_audio`` -- it already loads only
    ``[begin_time, end_time)`` via librosa's ``offset``/``duration``, no
    separate file I/O needed). ``audio_max_length_s`` overrides
    ``args.max_audio_s`` for that one call's feature-extractor truncation
    ceiling -- used by the windowed caller so each <=~28 s slice is sized
    correctly rather than compared against the full-clip 30 s default.
    """
    import torch

    # int8 (bitsandbytes) is weight-only quantization -- the audio feature
    # tensor fed into the model must stay a float compute dtype (the int8
    # weight tensors get dequantized to this dtype on the fly inside
    # Linear8bitLt). Casting the audio input itself to torch.int8 crashes
    # ("Input type (signed char) and bias type (BFloat16) should be the
    # same") -- confirmed on this ARK-3B/int8 lane 2026-07-18. bfloat16
    # matches the checkpoint's native dtype (the official card's own
    # recommended compute dtype for this model).
    audio_input_dtype = torch.bfloat16 if args.dtype == "int8" else getattr(torch, args.dtype)
    audio_part: dict[str, Any] = {"type": "audio", "path": row["path"]}
    if begin_time is not None:
        audio_part["begin_time"] = begin_time
    if end_time is not None:
        audio_part["end_time"] = end_time
    conversation = [
        {
            "role": "user",
            "content": [
                audio_part,
                {"type": "text", "text": TRANSCRIBE_PROMPT_EN},
            ],
        }
    ]

    effective_max_audio_s = audio_max_length_s if audio_max_length_s is not None else args.max_audio_s
    audio_max_length = int(effective_max_audio_s * 16000)
    inputs = processor.apply_chat_template(
        conversation,
        add_generation_prompt=True,
        return_tensors="pt",
        sampling_rate=16000,
        audio_padding="longest",
        text_kwargs={"padding": "longest"},
        audio_max_length=audio_max_length,
    )
    inputs = inputs.to(args.device)
    if "audios" in inputs:
        inputs["audios"] = inputs["audios"].to(dtype=audio_input_dtype)

    bad_words_ids = build_bad_words_ids(tokenizer)
    with torch.inference_mode():
        outputs = model.generate(
            **inputs,
            do_sample=False,
            max_new_tokens=args.max_new_tokens,
            pad_token_id=tokenizer.pad_token_id,
            eos_token_id=tokenizer.eos_token_id,
            bad_words_ids=bad_words_ids,
        )
    decoded = tokenizer.batch_decode(
        outputs[:, inputs["input_ids"].shape[1]:],
        skip_special_tokens=True,
    )
    hyp = decoded[0] if decoded else ""

    # Silent-truncation flag: the feature extractor truncates audio_max_length
    # samples with no exception (see module docstring). A windowed slice
    # (begin_time is not None) is an exact, deliberate cut of the full clip,
    # not data loss -- only the whole-clip single-shot path (begin_time is
    # None) can be legitimately flagged as truncated against the manifest's
    # duration_s. duration_s comes from the candidate-visible manifest, not a
    # re-measurement of the file, per the name-blind contract (adapters may
    # read duration_s for bookkeeping).
    if begin_time is not None:
        truncated = False
    else:
        truncated = float(row["duration_s"]) > effective_max_audio_s + 1e-6
    return hyp, truncated


def build_windows(duration_s: float, window_s: float, overlap_s: float) -> list[tuple[float, float]]:
    """Split ``[0, duration_s)`` into <=``window_s``-second windows with
    ``overlap_s`` seconds of overlap between consecutive windows. Returns a
    single ``[(0.0, duration_s)]`` window when the clip already fits."""
    if duration_s <= window_s + 1e-9:
        return [(0.0, duration_s)]
    stride = window_s - overlap_s
    if stride <= 0:
        raise ValueError("window_overlap_s must be smaller than window_s")
    windows: list[tuple[float, float]] = []
    start = 0.0
    while True:
        end = min(start + window_s, duration_s)
        windows.append((start, end))
        if end >= duration_s - 1e-6:
            break
        start += stride
    return windows


_PUNCT_STRIP = ".,!?;:\"'"


def dedupe_stitch(pieces: list[str]) -> str:
    """Merge overlapping-window transcripts into one string.

    Simple longest-suffix/prefix word-level overlap dedup: for each new
    piece, find the longest run (up to 40 words) where the tail of the
    merged transcript-so-far matches (case/punctuation-insensitive) the head
    of the new piece, and splice past that overlap rather than concatenating
    verbatim. ARK has no native word-timestamp output to align on, so this
    text-only heuristic is the documented stitch method for
    ``--long-form window`` -- it is not claimed to be lossless for every
    boundary (a genuinely different word pair that happens to collide is not
    detected), only a reasonable de-duplication of the ~2 s repeated-audio
    overlap region by design.
    """
    pieces = [p.strip() for p in pieces if p and p.strip()]
    if not pieces:
        return ""
    merged_words = pieces[0].split()
    for piece in pieces[1:]:
        words = piece.split()
        max_check = min(len(merged_words), len(words), 40)
        best_overlap = 0
        for k in range(max_check, 0, -1):
            suffix = [w.lower().strip(_PUNCT_STRIP) for w in merged_words[-k:]]
            prefix = [w.lower().strip(_PUNCT_STRIP) for w in words[:k]]
            if suffix == prefix:
                best_overlap = k
                break
        merged_words.extend(words[best_overlap:])
    return " ".join(merged_words)


def transcribe_row(
    processor: Any,
    tokenizer: Any,
    model: Any,
    spec: ArkModelSpec,
    args: argparse.Namespace,
    row: dict[str, Any],
) -> tuple[str, bool]:
    """Top-level per-row decode entry point (added 2026-07-18). Dispatches to
    the single-shot ``transcribe_one`` (unchanged raw/truncate behavior) or
    to the windowed multi-call + ``dedupe_stitch`` path, per
    ``args.long_form``. This is what ``process_rows`` calls instead of
    ``transcribe_one`` directly."""
    duration_s = float(row["duration_s"])
    if args.long_form == "truncate" or duration_s <= args.window_s + 1e-9:
        return transcribe_one(processor, tokenizer, model, spec, args, row)

    windows = build_windows(duration_s, args.window_s, args.window_overlap_s)
    pieces: list[str] = []
    for begin_time, end_time in windows:
        hyp, _truncated = transcribe_one(
            processor,
            tokenizer,
            model,
            spec,
            args,
            row,
            begin_time=begin_time,
            end_time=end_time,
            audio_max_length_s=(end_time - begin_time) + 1.0,  # small margin over the exact slice
        )
        pieces.append(hyp)
    merged = dedupe_stitch(pieces)
    # Windowed mode covers the full clip by construction -- never "truncated".
    return merged, False


# ==========================================================================
# Env record
# ==========================================================================
def build_env_record(
    spec: ArkModelSpec, args: argparse.Namespace, framework_version: str | None
) -> dict[str, Any]:
    gpu_info = detect_gpu()
    ram_mb = detect_ram_mb()
    non_default: dict[str, Any] = {}
    if args.max_audio_s != MAX_AUDIO_S_ARCHITECTURAL:
        non_default["max_audio_s"] = (
            f"{args.max_audio_s} (official/architectural default is "
            f"{MAX_AUDIO_S_ARCHITECTURAL}; values above this crash rather than "
            "extend support -- see ark_common.py MAX_AUDIO_S_ARCHITECTURAL)"
        )
    non_default["language_directive"] = (
        f'prompt text "{TRANSCRIBE_PROMPT_EN}" (model has no formal language '
        "parameter; English fixed via the instruction string itself, per the "
        "decode contract -- not content/vocabulary bias)"
    )

    windowed = args.long_form == "window"
    if windowed:
        chunk_size_s = args.window_s
        overlap_s = args.window_overlap_s
        stride_s = args.window_s - args.window_overlap_s  # hop between window starts
        reassembly = "window_textmerge"  # dedupe_stitch: word-level longest-suffix/prefix overlap merge
        non_default["long_form"] = (
            f"window (chunk_size_s={chunk_size_s}, overlap_s={overlap_s}, stride_s={stride_s}: "
            "documented product-eval chunking variant, NOT the raw/truncate comparator used for "
            "the frozen Pico-224 run -- see ark_common.py WINDOW_S_DEFAULT docstring)"
        )
    else:
        chunk_size_s = None
        overlap_s = None
        stride_s = None
        reassembly = None

    return {
        "framework": "transformers",
        "framework_version": framework_version,
        "model_revision": spec.pinned_revision,
        "dtype": args.dtype,
        "device": args.device,
        "attn_impl": args.attn_impl,
        "decode_mode": "greedy",
        "beam_width": 1,
        "temperature": None,
        "lang_token": "en",
        "task_token": "transcribe",
        "punct_case_flag": "model_default",
        "max_input_s": args.max_audio_s,
        "max_new_tokens": args.max_new_tokens,
        "batch_size": 1,
        "vad": "none",
        "waveform_norm": "none",
        "chunk_size_s": chunk_size_s,
        "stride_s": stride_s,
        "overlap_s": overlap_s,
        "reassembly": reassembly,
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
    processor: Any,
    tokenizer: Any,
    model: Any,
    spec: ArkModelSpec,
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
            transcribe_row(processor, tokenizer, model, spec, args, rows[0])
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
                    hyp_text, truncated = transcribe_row(processor, tokenizer, model, spec, args, row)
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
                # value (see adapters/_contract.md). A >30s clip run with an
                # enlarged --max-audio-s hits this path via the encoder
                # position-embedding shape mismatch documented in the module
                # docstring, not "out of memory".
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
def build_arg_parser(spec: ArkModelSpec) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=f"Bakeoff adapter for {spec.repo_id}")
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(__file__).resolve().parents[2] / "out" / spec.model_key,
    )
    parser.add_argument("--smoke-ids", type=str, default=None, help="comma-separated row ids")
    parser.add_argument("--full", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--device", choices=["cuda", "cpu"], default="cuda")
    parser.add_argument("--model-dir", type=Path, default=spec.default_model_dir)
    parser.add_argument(
        "--dtype",
        choices=sorted(BYTES_PER_PARAM),
        default=None,  # resolved to spec.default_dtype_cuda / float32 below, per official per-device recommendation
    )
    parser.add_argument("--attn-impl", default="sdpa")
    parser.add_argument(
        "--max-audio-s",
        type=float,
        default=MAX_AUDIO_S_ARCHITECTURAL,
        help=(
            "audio_max_length passed to apply_chat_template, in seconds. "
            "Default matches the architectural cap (see module docstring); "
            "raising it crashes rather than extends support."
        ),
    )
    parser.add_argument("--max-new-tokens", type=int, default=MAX_NEW_TOKENS_DEFAULT)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument(
        "--long-form",
        choices=["window", "truncate"],
        default="window",
        help=(
            "window (default): split clips over --window-s into overlapping "
            "windows and stitch transcripts (dedupe_stitch) -- documented "
            "product-eval chunking variant. truncate: single-shot call, "
            "silent truncation past --max-audio-s (the raw/comparator mode "
            "used for the frozen Pico-224 run)."
        ),
    )
    parser.add_argument("--window-s", type=float, default=WINDOW_S_DEFAULT)
    parser.add_argument("--window-overlap-s", type=float, default=WINDOW_OVERLAP_S_DEFAULT)
    return parser


def _try_resolve_snapshot_dir_for_report(model_dir: Path, spec: ArkModelSpec) -> str | None:
    try:
        return str(resolve_snapshot_dir(model_dir, spec))
    except SystemExit:
        return None
    except Exception:
        return None


def dry_run(spec: ArkModelSpec, args: argparse.Namespace, rows: list[dict[str, Any]]) -> int:
    resolved = {
        "model_key": spec.model_key,
        "repo_id": spec.repo_id,
        "pinned_revision": spec.pinned_revision,
        "model_dir": str(args.model_dir),
        "model_dir_exists": args.model_dir.exists(),
        "resolved_snapshot_dir": _try_resolve_snapshot_dir_for_report(args.model_dir, spec),
        "out_dir": str(args.out_dir),
        "manifest": str(args.manifest),
        "manifest_rows": len(rows),
        "device_requested": args.device,
        "dtype": args.dtype,
        "attn_impl": args.attn_impl,
        "max_audio_s": args.max_audio_s,
        "max_new_tokens": args.max_new_tokens,
        "language_directive": TRANSCRIBE_PROMPT_EN,
        "seed": args.seed,
        "architectural_cap_s": MAX_AUDIO_S_ARCHITECTURAL,
        "contract_max_smoke_clip_s": MAX_AUDIO_S_CONTRACT,
        "long_form": args.long_form,
        "window_s": args.window_s if args.long_form == "window" else None,
        "window_overlap_s": args.window_overlap_s if args.long_form == "window" else None,
        "cap_risk": (
            (
                f"clips over {args.max_audio_s:g}s WILL be silently truncated (status: "
                "truncation), not chunked -- no documented long-form path exists for "
                "this model family natively. The fixed 5-item smoke set includes 3 clips "
                f"over this cap (24.51s, 77.15s, 110.75s) -- expect status=truncation on "
                "all three at the default --max-audio-s=30."
            )
            if args.long_form == "truncate"
            else (
                f"--long-form window (default): clips over {args.window_s:g}s are split "
                f"into <= {args.window_s:g}s windows ({args.window_overlap_s:g}s overlap) "
                "and stitched with dedupe_stitch (word-level text-overlap merge) -- a "
                "documented product-eval chunking variant added 2026-07-18, distinct from "
                "the frozen truncate-mode Pico-224 comparator result above. status=ok "
                "expected for the full-length smoke clips, not truncation."
            )
        ),
    }
    preflight = preflight_vram_estimate(args.model_dir, args.dtype, spec, args.max_new_tokens)
    print(json.dumps({"resolved_config": resolved, "preflight_8gb": preflight}, indent=2))
    return 0


def main(spec: ArkModelSpec, argv: list[str] | None = None) -> int:
    parser = build_arg_parser(spec)
    args = parser.parse_args(argv)
    if args.dtype is None:
        args.dtype = spec.default_dtype_cuda if args.device == "cuda" else "float32"
    args.smoke_ids = [s.strip() for s in args.smoke_ids.split(",")] if args.smoke_ids else None

    if not args.manifest.exists():
        print(f"manifest not found: {args.manifest}", file=sys.stderr)
        return 2

    rows = load_manifest_rows(args.manifest)

    if args.dry_run:
        return dry_run(spec, args, rows)

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
    processor, tokenizer, model = load_model(spec, args)
    cold_load_s = time.perf_counter() - t_load0

    import transformers

    env_record = build_env_record(spec, args, transformers.__version__)
    env_path.write_text(json.dumps(env_record, indent=2), encoding="utf-8")

    rows_done, worst, retries_used = process_rows(
        processor, tokenizer, model, spec, args, selected, hyp_path, status_path, cold_load_s
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


def run_cli(spec: ArkModelSpec) -> int:
    return main(spec)
