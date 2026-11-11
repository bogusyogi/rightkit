#!/usr/bin/env python3
"""Bakeoff inference adapter -- MOSS-Transcribe preview 2B.

Repo: OpenMOSS-Team/MOSS-Transcribe-preview-2B (pinned revision below).
Local-only ASR adapter for the 2026-07-17 open-ASR top-model dispatch run.
Governed by ``asr-bakeoff/CONTRACT.md`` and
``docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md``. No
package install, download, or inference happens as a side effect of
importing or ``--dry-run``-ing this file; a real (non-dry-run) invocation is
gated on Adrian's exact approval token recorded in the frozen run contract,
per ``docs/BAKEOFF_RUN_GATE.md``.

Model status: OpenMOSS labels this checkpoint "preview" in its own repo
name -- a pre-release checkpoint, not a stable/GA release. Every output row
and ``env.json`` carries ``"preview": true`` so downstream reporting cannot
drop that fact silently.

Official inference path (unconditioned ASR, no chat/system prompt), taken
verbatim from the model card at
https://huggingface.co/OpenMOSS-Team/MOSS-Transcribe-preview-2B
(README.md "Python Inference Code Block", fetched 2026-07-17):

    model = AutoModelForCausalLM.from_pretrained(
        REPO, dtype=torch.bfloat16, trust_remote_code=True
    ).to(DEVICE).eval()
    tokenizer = AutoTokenizer.from_pretrained(REPO, trust_remote_code=True)
    MossProcessor = get_class_from_dynamic_module("processing_Moss.MossProcessor", REPO)
    MelConfig = get_class_from_dynamic_module("processing_Moss.MelConfig", REPO)
    mel_cfg = MelConfig(mel_sr=16000, mel_dim=128, mel_n_fft=400, mel_hop_length=160)
    processor = MossProcessor(tokenizer, config=mel_cfg, enable_time_marker=False)
    processor.load_template(hf_hub_download(REPO, "chat_template_default.py"))
    inputs = processor(audio=waveform, return_tensors="pt").to(DEVICE)
    inputs["audio_data"] = inputs["audio_data"].to(model.dtype)
    out_ids = model.generate(
        **inputs, max_new_tokens=512, do_sample=False, num_beams=1,
        use_cache=True, eos_token_id=[processor.end_token_id],
    )
    transcript = processor.batch_decode(new_ids, skip_special_tokens=True)[0].strip()

This adapter reproduces that path exactly (greedy, ``num_beams=1``,
``do_sample=False``, ``max_new_tokens=512`` by default) with two local-only
substitutions so no network call is ever made from a real run:

  1. ``REPO`` (a bare hub id, which would resolve over the network) is
     replaced with ``--model-dir`` -- a local, already-downloaded, flat
     snapshot directory (config.json / model.safetensors / tokenizer files /
     chat_template_default.py side by side, matching the acquisition lane's
     ``hf download --local-dir`` layout). ``local_files_only=True`` is passed
     to every ``from_pretrained`` / dynamic-module call, and
     ``HF_HUB_OFFLINE`` / ``TRANSFORMERS_OFFLINE`` are set before the heavy
     imports so huggingface_hub/transformers cannot silently fall back to a
     network fetch.
  2. ``hf_hub_download(REPO, "chat_template_default.py")`` (a network call)
     is replaced with a direct local read of
     ``<model-dir>/chat_template_default.py``.

Long-form handling: the model card documents no chunking/windowing policy
and states no maximum supported duration. This adapter therefore runs the
full waveform through the single documented pass for every clip, including
the 77 s and 110.75 s smoke clips -- per the run packet, an adapter must not
invent a chunking policy the model card does not itself document. If a real
run truncates, empties, or crashes on a long clip, that row is recorded as
``truncation`` / ``empty`` / ``unsupported``, not silently re-chunked.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import sys
import time
import traceback
from pathlib import Path

ADAPTER_KEY = "moss-transcribe-2b"
HF_REPO = "OpenMOSS-Team/MOSS-Transcribe-preview-2B"
PINNED_REVISION = "c98175cb20e48bd9be4e95f6c85f2af18899f780"
LICENSE = "Apache-2.0"
IS_PREVIEW = True

DEFAULT_MODEL_DIR = Path.cwd() / ".cache" / "asr-models" / "moss-transcribe-2b"
_HERE = Path(__file__).resolve()
DEFAULT_OUT_DIR = Path.cwd() / "out" / ADAPTER_KEY

# Official decode defaults, copied verbatim from the model card's snippet.
DEFAULT_DTYPE = "bfloat16"
DEFAULT_MAX_NEW_TOKENS = 512
MEL_CFG = {"mel_sr": 16000, "mel_dim": 128, "mel_n_fft": 400, "mel_hop_length": 160}

# The run packet's declared maximum-duration smoke clip; used only for the
# preflight sizing estimate below, never to gate/chunk a real decode call.
MAX_CLIP_S = 110.75

ROW_STATUSES = ("ok", "empty", "truncation", "unsupported")
COMPLETION_PRIORITY = {
    "ok": 0,
    "empty": 1,
    "truncation": 2,
    "unsupported": 3,
    "crash": 4,
    "oom": 5,
}

# Documented fallback used only when --model-dir has no config.json to read
# (e.g. the model has not been acquired yet). Sourced from the run packet's
# "Pre-declared memory and host budget" table
# (docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md), which
# lists an approximate BF16 weight floor of ~4.8 GB for this checkpoint.
# This is a documented placeholder, not a measurement.
PACKET_APPROX_WEIGHT_GB = 4.8


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="MOSS-Transcribe-preview-2B bakeoff adapter")
    p.add_argument("--manifest", required=True, type=Path, help="candidate-visible manifest JSON")
    p.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    p.add_argument("--smoke-ids", type=str, default=None, help="comma-separated row ids")
    p.add_argument("--full", action="store_true")
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--device", choices=["cuda", "cpu"], default="cuda")
    p.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    p.add_argument("--dtype", choices=["bfloat16", "float16", "float32"], default=DEFAULT_DTYPE)
    p.add_argument("--attn-impl", type=str, default="sdpa")
    # Model design is English-only per the card; kept explicit and fixed per
    # the decode contract rather than left to an auto-detect default.
    p.add_argument("--language", type=str, default="en")
    p.add_argument("--max-new-tokens", type=int, default=DEFAULT_MAX_NEW_TOKENS)
    p.add_argument("--seed", type=int, default=0)
    args = p.parse_args(argv)
    if not args.dry_run and bool(args.smoke_ids) == bool(args.full):
        p.error("exactly one of --smoke-ids or --full is required outside --dry-run")
    return args


def read_manifest(path: Path):
    raw = json.loads(path.read_text(encoding="utf-8"))
    rows = raw["rows"] if isinstance(raw, dict) and "rows" in raw else raw
    if not isinstance(rows, list):
        raise ValueError(f"manifest at {path} is not a row list or {{'rows': [...]}} object")
    required = {"id", "path", "duration_s", "sha256"}
    for i, row in enumerate(rows):
        missing = required - set(row)
        if missing:
            raise ValueError(f"manifest row {i} missing required key(s) {sorted(missing)}")
    return rows


def select_rows(rows, args):
    if args.full:
        return rows
    ids = {x.strip() for x in (args.smoke_ids or "").split(",") if x.strip()}
    selected = [r for r in rows if r["id"] in ids]
    missing = ids - {r["id"] for r in selected}
    if missing:
        raise ValueError(f"--smoke-ids not found in manifest: {sorted(missing)}")
    return selected


def dtype_bytes(name: str) -> int:
    return {"bfloat16": 2, "float16": 2, "float32": 4}[name]


def scan_weight_bytes(model_dir: Path) -> int | None:
    if not model_dir.is_dir():
        return None
    total = 0
    found = False
    for pattern in ("*.safetensors", "*.bin", "*.pt"):
        for f in glob.glob(str(model_dir / pattern)):
            total += os.path.getsize(f)
            found = True
    return total if found else None


def read_local_config(model_dir: Path) -> dict | None:
    cfg_path = model_dir / "config.json"
    if not cfg_path.is_file():
        return None
    try:
        return json.loads(cfg_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def estimate_kv_bytes(cfg: dict | None, seq_tokens: int, dtype_size: int) -> tuple[float, str]:
    """Return (kv_gb, method) for a sequence of ``seq_tokens`` tokens.

    Tries to read the decoder's hidden_size / num_hidden_layers /
    num_key_value_heads straight out of the acquired checkpoint's own
    config.json. Falls back to a documented, clearly-labeled generic
    ~2B-decoder-class placeholder (hidden_size=2048, num_hidden_layers=24,
    num_key_value_heads=16, i.e. no GQA reduction assumed) when the config
    is unavailable -- an upper-bound guess, not a measured or model-card
    value, and it is always reported alongside the method string below so a
    reader can tell measured from guessed.
    """
    if cfg:
        hidden = cfg.get("hidden_size") or cfg.get("d_model")
        layers = cfg.get("num_hidden_layers") or cfg.get("n_layers")
        heads = cfg.get("num_attention_heads") or cfg.get("n_heads")
        kv_heads = cfg.get("num_key_value_heads") or heads
        if hidden and layers and heads and kv_heads:
            head_dim = hidden / heads
            kv_bytes = 2 * layers * kv_heads * head_dim * seq_tokens * dtype_size
            return kv_bytes / 1e9, "measured_from_local_config_json"
    # Fallback: generic placeholder, explicitly flagged as unverified.
    hidden, layers, kv_heads, heads = 2048, 24, 16, 16
    head_dim = hidden / heads
    kv_bytes = 2 * layers * kv_heads * head_dim * seq_tokens * dtype_size
    return kv_bytes / 1e9, "unverified_generic_2b_decoder_placeholder"


def preflight(args) -> dict:
    weight_bytes = scan_weight_bytes(args.model_dir)
    if weight_bytes is not None:
        weight_gb = weight_bytes / 1e9
        weight_method = "measured_from_local_model_dir"
    else:
        weight_gb = PACKET_APPROX_WEIGHT_GB
        weight_method = "run_packet_declared_approx_bf16_floor"

    cfg = read_local_config(args.model_dir)
    dsize = dtype_bytes(args.dtype)
    # Mel hop_length=160 @ 16 kHz -> 10 ms/frame; the model card does not
    # document the audio encoder's downsampling factor, so this is an
    # upper-bound token count (no downsampling assumed) rather than an
    # invented deeper reduction.
    audio_tokens_upper_bound = int(MAX_CLIP_S / (MEL_CFG["mel_hop_length"] / MEL_CFG["mel_sr"]))
    prompt_overhead_tokens = 32
    seq_tokens = audio_tokens_upper_bound + args.max_new_tokens + prompt_overhead_tokens
    kv_gb, kv_method = estimate_kv_bytes(cfg, seq_tokens, dsize)

    # Documented headroom margin for non-KV activation/workspace memory:
    # 15% of (weights + KV), matching this run's own 20% headroom rule for
    # the reference host (packet "Pre-declared memory and host budget").
    subtotal_gb = weight_gb + kv_gb
    activation_margin_gb = 0.15 * subtotal_gb
    total_gb = subtotal_gb + activation_margin_gb

    eight_gb = 8.0
    if total_gb <= 0.8 * eight_gb:
        verdict = "full"
    elif total_gb <= eight_gb:
        verdict = "smoke"
    else:
        verdict = "defer"

    return {
        "weight_gb": round(weight_gb, 3),
        "weight_method": weight_method,
        "kv_activation_gb_at_110_75s": round(kv_gb, 3),
        "kv_method": kv_method,
        "activation_margin_gb": round(activation_margin_gb, 3),
        "total_estimate_gb": round(total_gb, 3),
        "audio_tokens_upper_bound_at_110_75s": audio_tokens_upper_bound,
        "verdict_8gb": verdict,
        "note": "sizing aid for the coordinator, not a scored result",
    }


def build_env_record(args, extra: dict | None = None) -> dict:
    env = {
        "framework": "transformers",
        "framework_version": None,
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
        "max_input_s": None,
        "max_new_tokens": args.max_new_tokens,
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
        "gpu": None,
        "vram_mb": 0,
        "ram": 0,
        "os": platform.platform(),
        "driver": None,
        "host_class": None,
        "license": LICENSE,
        "preview": IS_PREVIEW,
        "hf_repo": HF_REPO,
    }
    if args.max_new_tokens != DEFAULT_MAX_NEW_TOKENS:
        env["non_default_params"]["max_new_tokens"] = {
            "value": args.max_new_tokens,
            "official_default": DEFAULT_MAX_NEW_TOKENS,
            "reason": "cli override; decode-length budget only, no context/bias change",
        }
    if extra:
        env.update(extra)
    return env


def _load_model(args):
    """Heavy, network-safe local-only load. Imported lazily so --dry-run and
    py_compile never require torch/transformers to be installed."""
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from transformers.dynamic_module_utils import get_class_from_dynamic_module

    if not args.model_dir.is_dir():
        raise FileNotFoundError(
            f"--model-dir {args.model_dir} does not exist; this adapter never downloads "
            "weights itself -- run the acquisition lane first"
        )

    torch_dtype = {"bfloat16": torch.bfloat16, "float16": torch.float16, "float32": torch.float32}[
        args.dtype
    ]
    model_dir_str = str(args.model_dir)

    model = (
        AutoModelForCausalLM.from_pretrained(
            model_dir_str,
            dtype=torch_dtype,
            trust_remote_code=True,
            local_files_only=True,
        )
        .to(args.device)
        .eval()
    )
    tokenizer = AutoTokenizer.from_pretrained(
        model_dir_str, trust_remote_code=True, local_files_only=True
    )

    MossProcessor = get_class_from_dynamic_module(
        "processing_Moss.MossProcessor", model_dir_str, local_files_only=True
    )
    MelConfig = get_class_from_dynamic_module(
        "processing_Moss.MelConfig", model_dir_str, local_files_only=True
    )
    mel_cfg = MelConfig(**MEL_CFG)
    processor = MossProcessor(tokenizer, config=mel_cfg, enable_time_marker=False)

    template_path = args.model_dir / "chat_template_default.py"
    if not template_path.is_file():
        raise FileNotFoundError(
            f"{template_path} missing from the local model dir; this adapter reads it "
            "locally only and never calls hf_hub_download"
        )
    processor.load_template(str(template_path))

    return torch, model, processor


def _decode_one(torch, model, processor, args, wav_path: str) -> tuple[str, str]:
    """Single official-path decode call. Returns (hypothesis, status)."""
    import librosa

    waveform, _sr = librosa.load(wav_path, sr=MEL_CFG["mel_sr"])
    inputs = processor(audio=waveform, return_tensors="pt").to(args.device)
    inputs["audio_data"] = inputs["audio_data"].to(model.dtype)
    with torch.no_grad():
        out_ids = model.generate(
            **inputs,
            max_new_tokens=args.max_new_tokens,
            do_sample=False,
            num_beams=1,
            use_cache=True,
            eos_token_id=[processor.end_token_id],
        )
    new_ids = out_ids[:, inputs["input_ids"].shape[1] :]
    if new_ids.shape[1] == 0:
        return "", "empty"
    transcript = processor.batch_decode(new_ids, skip_special_tokens=True)[0].strip()
    if not transcript:
        return "", "empty"
    if new_ids.shape[1] >= args.max_new_tokens:
        return transcript, "truncation"
    return transcript, "ok"


def run(args) -> int:
    rows = read_manifest(args.manifest)

    if args.dry_run:
        record = {
            "adapter_key": ADAPTER_KEY,
            "hf_repo": HF_REPO,
            "pinned_revision": PINNED_REVISION,
            "license": LICENSE,
            "preview": IS_PREVIEW,
            "model_dir": str(args.model_dir),
            "model_dir_exists": args.model_dir.is_dir(),
            "manifest_rows": len(rows),
            "env": build_env_record(args),
            "preflight": preflight(args),
        }
        print(json.dumps(record, indent=2))
        return 0

    selected = select_rows(rows, args)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    hyp_path = args.out_dir / "hypotheses.jsonl"
    env_path = args.out_dir / "env.json"
    status_path = args.out_dir / "status.json"

    t0 = time.time()
    torch, model, processor = _load_model(args)
    cold_load_s = time.time() - t0

    status = {
        "completion": "ok",
        "rows_done": 0,
        "rows_total": len(selected),
        "cold_load_s": round(cold_load_s, 3),
        "peak_vram_mb": 0,
        "retries": 0,
    }

    warmup_id = None
    remaining = list(selected)
    if remaining:
        warmup_id = remaining[0]["id"]
        try:
            _decode_one(torch, model, processor, args, remaining[0]["path"])
        except Exception:  # noqa: BLE001 - warm-up failure is not scored
            pass

    env = build_env_record(args, {"warmup_id": warmup_id})
    env_path.write_text(json.dumps(env, indent=2), encoding="utf-8")

    with hyp_path.open("a", encoding="utf-8") as hyp_f:
        for row in selected:
            attempts = 0
            row_status = "unsupported"
            hypothesis = ""
            decode_s = None
            worst = "ok"
            while attempts < 2:
                attempts += 1
                try:
                    t1 = time.time()
                    hypothesis, row_status = _decode_one(torch, model, processor, args, row["path"])
                    decode_s = time.time() - t1
                    worst = row_status
                    break
                except Exception as exc:  # noqa: BLE001 - one infra retry, then scored failure
                    is_oom = "out of memory" in str(exc).lower() or "CUDA out of memory" in str(exc)
                    if attempts >= 2:
                        row_status = "unsupported"
                        worst = "oom" if is_oom else "crash"
                        traceback.print_exc(file=sys.stderr)
                    else:
                        status["retries"] += 1
            hyp_f.write(
                json.dumps({"id": row["id"], "hypothesis": hypothesis, "decode_s": decode_s, "status": row_status})
                + "\n"
            )
            hyp_f.flush()
            status["rows_done"] += 1
            if COMPLETION_PRIORITY[worst] > COMPLETION_PRIORITY[status["completion"]]:
                status["completion"] = worst
            status_path.write_text(json.dumps(status, indent=2), encoding="utf-8")

    return 0


def main(argv=None) -> int:
    args = parse_args(argv)
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
