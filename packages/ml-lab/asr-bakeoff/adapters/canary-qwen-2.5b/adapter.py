#!/usr/bin/env python3
"""Bakeoff inference adapter -- Canary-Qwen 2.5B (NeMo SALM, ASR mode).

Repo: nvidia/canary-qwen-2.5b (pinned revision below). Local-only ASR
adapter for the 2026-07-17 open-ASR top-model dispatch run. Governed by
``asr-bakeoff/CONTRACT.md`` and
``docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md``. No
package install, download, or inference happens as a side effect of
importing or ``--dry-run``-ing this file. Per that run packet's roster
table, Canary-Qwen is deferred to a separate larger-host packet for the
zero-paid local run -- this adapter is authored and preflight-sized now so
that packet does not need to re-derive the ASR-mode call shape or the
long-form windowing policy from scratch. A real (non-dry-run) invocation is
additionally gated on Adrian's exact approval token recorded in the frozen
run contract, per ``docs/BAKEOFF_RUN_GATE.md``.

ASR mode, not LLM/GER mode: Canary-Qwen is a Speech-Augmented Language
Model (SALM) -- a FastConformer audio encoder feeding a Qwen3-1.7B decoder
through a modality adapter. The same class can be prompted for open-ended
LLM tasks (its "GER" / generative-error-repair mode); this adapter only
ever issues the model's documented ASR-decode call, never a follow-up LLM
turn, and never injects any context/vocabulary/bias text.

Official inference path, taken from the model card at
https://huggingface.co/nvidia/canary-qwen-2.5b (fetched 2026-07-17):

    from nemo.collections.speechlm2.models import SALM
    model = SALM.from_pretrained('nvidia/canary-qwen-2.5b')
    answer_ids = model.generate(
        prompts=[[{
            "role": "user",
            "content": f"Transcribe the following: {model.audio_locator_tag}",
            "audio": ["speech.wav"],
        }]],
        max_new_tokens=128,
    )
    transcription = model.tokenizer.ids_to_text(answer_ids[0].cpu())

The literal string ``"Transcribe the following: "`` immediately followed by
the model's own ``audio_locator_tag`` is the model's documented, fixed
instruction scaffold for selecting ASR-decode behavior over its chat/LLM
interface -- it is identical for every row, carries no clip identity, and
is not treated as injected context/vocabulary/bias for the purposes of the
run packet's decode contract. This adapter never varies, appends to, or
conditions that string on manifest content.

This adapter reproduces the official call with one local-only substitution:
``SALM.from_pretrained('nvidia/canary-qwen-2.5b')`` (a bare hub id, which
would resolve over the network) is replaced with
``SALM.from_pretrained(str(model_dir))`` against ``--model-dir`` -- a local,
already-downloaded, flat snapshot directory (config.json / model.safetensors
side by side, matching the acquisition lane's ``hf download --local-dir``
layout) -- with ``HF_HUB_OFFLINE`` / ``TRANSFORMERS_OFFLINE`` set before the
heavy imports so no network fetch can occur. NeMo's speechlm2 model loading
is built on the same huggingface_hub resolution transformers uses, so a
local directory path with offline mode enabled is expected to resolve
purely from disk; this specific local-directory code path is not directly
confirmed against a live install (Canary is deferred, so this has not been
exercised), and that uncertainty is called out again in this lane's
README.md.

Long-form handling (mandatory -- the model was trained on <=40 s clips):
the model card states "the maximum audio duration in training was 40s" and
that its own long-form evaluation (CasualConversations-v1) was run
"on non-overlapping 40s chunks". This adapter implements exactly that
documented policy for any clip longer than 40 s: split into non-overlapping
40.0 s windows (final window shorter), run the identical official ASR call
per window, and reassemble by concatenating the per-window transcripts in
order, joined by a single space. The model card does not document a
smoothing/stitching algorithm beyond "non-overlapping chunks", so no
overlap-based merge is invented here -- concatenation-in-order is the
simplest reassembly consistent with "non-overlapping". Every chunk
boundary and this reassembly choice is written to ``env.json`` so it can
be audited or challenged.

A separate, NOT used, alternative exists upstream: NVIDIA-NeMo/NeMo PR
#15364 ("Add buffered inference support for SALM models", merged
2026-02-09) adds an overlapping streaming-buffer inference path
(buffer=8.0s / chunk=2.0s / overlap=4.0s, LCS-based token stitching) and is
explicitly marked experimental / not production-ready by its authors. This
adapter does not use it: it depends on a NeMo version newer than this
packet pins, and the model card's own documented 40s-chunk methodology is
the more clearly specified, non-experimental option for an "official/
recommended unconditioned" path.
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import platform
import sys
import tempfile
import time
import traceback
from pathlib import Path

ADAPTER_KEY = "canary-qwen-2.5b"
HF_REPO = "nvidia/canary-qwen-2.5b"
PINNED_REVISION = "b1469e1bba1cfe140205529c79c434ca47180960"
LICENSE = "CC-BY-4.0"
ATTRIBUTION = (
    "NVIDIA Canary-Qwen-2.5B (nvidia/canary-qwen-2.5b), used under CC-BY-4.0: "
    "https://huggingface.co/nvidia/canary-qwen-2.5b -- "
    "https://creativecommons.org/licenses/by/4.0/"
)

DEFAULT_MODEL_DIR = Path.cwd() / ".cache" / "asr-models" / "canary-qwen-2.5b"
_HERE = Path(__file__).resolve()
DEFAULT_OUT_DIR = Path.cwd() / "out" / ADAPTER_KEY

DEFAULT_DTYPE = "bfloat16"
SAMPLE_RATE = 16000

# Model-card-documented training/eval limit; see module docstring.
MAX_TRAINED_CLIP_S = 40.0
CHUNK_S = 40.0
STRIDE_S = 40.0  # == CHUNK_S -> non-overlapping, per the model card's own eval methodology
OVERLAP_S = 0.0
REASSEMBLY = "concat_in_order_space_joined"

# The official demo's max_new_tokens=128 is a short calibration default,
# not a documented per-window budget. FastConformer's frame rate is
# 80 ms/token (10 ms stride * subsampling_factor 8, both in the acquired
# checkpoint's own audio_encoder config -- see estimate_kv_bytes() below),
# so a 40 s window is ~500 audio tokens; against the model's own documented
# 1024-token total sequence budget (prompt + audio + response), 128 leaves
# an unnecessarily large truncation risk for dense 40 s speech. This
# adapter instead defaults to 400, leaving ~1024-500-~15(prompt)=~509
# tokens of headroom and logging the deviation as a non_default_param
# (a decode-length safety margin, not a context/bias change).
OFFICIAL_DEMO_MAX_NEW_TOKENS = 128
DEFAULT_MAX_NEW_TOKENS = 400
MAX_SEQ_TOKENS = 1024
AUDIO_TOKENS_PER_S = 1.0 / 0.08  # 80 ms/token

ASR_INSTRUCTION_PREFIX = "Transcribe the following: "

ROW_STATUSES = ("ok", "empty", "truncation", "unsupported")
COMPLETION_PRIORITY = {
    "ok": 0,
    "empty": 1,
    "truncation": 2,
    "unsupported": 3,
    "crash": 4,
    "oom": 5,
}

# Documented fallback used only when --model-dir has no config.json/weight
# files to measure (e.g. not yet acquired). Sourced from the run packet's
# "Pre-declared memory and host budget" table
# (docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md), which
# lists an approximate BF16 weight floor of ~5.0 GB, and the HF repo's own
# file listing shows a single ~5.12 GB model.safetensors. This is a
# documented placeholder, not a measurement.
PACKET_APPROX_WEIGHT_GB = 5.0

# The same packet table already dispositions this candidate as
# "Prior 8 GB swap; no full local run" for the 8 GB Windows host, based on
# evidence outside this adapter. This is surfaced (not silently discarded)
# alongside this adapter's own formula-based estimate below rather than
# overwritten by it.
PRIOR_8GB_DISPOSITION = "prior_evidence_8gb_swap_no_full_local_run"


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="Canary-Qwen-2.5B (NeMo SALM, ASR mode) bakeoff adapter")
    p.add_argument("--manifest", required=True, type=Path, help="candidate-visible manifest JSON")
    p.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    p.add_argument("--smoke-ids", type=str, default=None, help="comma-separated row ids")
    p.add_argument("--full", action="store_true")
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--device", choices=["cuda", "cpu"], default="cuda")
    p.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    p.add_argument("--dtype", choices=["bfloat16", "float16", "float32"], default=DEFAULT_DTYPE)
    p.add_argument("--attn-impl", type=str, default="sdpa")
    # ASR mode has no separate language switch in the documented call; the
    # decode contract still requires this to be recorded explicitly.
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
    for pattern in ("*.safetensors", "*.bin", "*.pt", "*.nemo"):
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


def window_bounds(duration_s: float):
    """Non-overlapping [start, end) windows of CHUNK_S, per the model card's
    own documented long-form evaluation methodology (module docstring)."""
    n = max(1, math.ceil(duration_s / CHUNK_S))
    bounds = []
    for i in range(n):
        start = i * CHUNK_S
        end = min(duration_s, start + CHUNK_S)
        bounds.append((start, end))
    return bounds


def estimate_kv_bytes(seq_tokens: int, dtype_size: int) -> tuple[float, str]:
    """KV-cache estimate for the Qwen3-1.7B decoder backbone.

    canary-qwen-2.5b's own config.json only references
    ``pretrained_llm: "Qwen/Qwen3-1.7B"`` rather than embedding the decoder
    dims directly, so this uses Qwen3-1.7B's published architecture
    (hidden_size=2048, num_hidden_layers=28, head_dim=128,
    num_key_value_heads=8) rather than the checkpoint's own config file --
    flagged as such in the returned method string.
    """
    hidden, layers, kv_heads, head_dim = 2048, 28, 8, 128
    kv_bytes = 2 * layers * kv_heads * head_dim * seq_tokens * dtype_size
    return kv_bytes / 1e9, "qwen3_1_7b_published_architecture_not_local_config"


def preflight(args) -> dict:
    weight_bytes = scan_weight_bytes(args.model_dir)
    if weight_bytes is not None:
        weight_gb = weight_bytes / 1e9
        weight_method = "measured_from_local_model_dir"
    else:
        weight_gb = PACKET_APPROX_WEIGHT_GB
        weight_method = "run_packet_declared_approx_bf16_floor"

    dsize = dtype_bytes(args.dtype)
    # Per-window sequence length bound (this adapter never exceeds one
    # 40 s window in a single forward pass -- see CHUNK_S above).
    audio_tokens_per_window = int(CHUNK_S * AUDIO_TOKENS_PER_S)
    prompt_overhead_tokens = 15  # "Transcribe the following: " + audio_locator_tag + special tokens
    seq_tokens = audio_tokens_per_window + args.max_new_tokens + prompt_overhead_tokens
    kv_gb, kv_method = estimate_kv_bytes(seq_tokens, dsize)

    # Documented headroom margin for non-KV activation/workspace memory
    # (FastConformer encoder activations, allocator fragmentation, CUDA/
    # NeMo runtime base overhead): 20% of (weights + KV), matching this
    # run's own 20% reference-host headroom rule (packet "Pre-declared
    # memory and host budget").
    subtotal_gb = weight_gb + kv_gb
    activation_margin_gb = 0.20 * subtotal_gb
    total_gb = subtotal_gb + activation_margin_gb

    eight_gb = 8.0
    if total_gb <= 0.8 * eight_gb:
        formula_verdict = "full"
    elif total_gb <= eight_gb:
        formula_verdict = "smoke"
    else:
        formula_verdict = "defer"

    # This adapter's own formula is a rough per-window estimate; it is not
    # allowed to silently overrule the run packet's already-declared prior
    # evidence for this exact candidate on an 8 GB Windows host. The more
    # conservative of the two is reported as the final verdict, with both
    # inputs shown.
    verdict_rank = {"full": 0, "smoke": 1, "defer": 2}
    prior_verdict = "defer"  # "prior 8 GB swap; no full local run" == defer, per the packet table
    final_verdict = formula_verdict if verdict_rank[formula_verdict] >= verdict_rank[prior_verdict] else prior_verdict

    return {
        "weight_gb": round(weight_gb, 3),
        "weight_method": weight_method,
        "kv_activation_gb_per_40s_window": round(kv_gb, 3),
        "kv_method": kv_method,
        "activation_margin_gb": round(activation_margin_gb, 3),
        "total_estimate_gb_per_window": round(total_gb, 3),
        "audio_tokens_per_40s_window": audio_tokens_per_window,
        "formula_verdict_8gb": formula_verdict,
        "prior_evidence_disposition": PRIOR_8GB_DISPOSITION,
        "verdict_8gb": final_verdict,
        "note": (
            "sizing aid for the coordinator, not a scored result; final verdict is the "
            "more conservative of this adapter's per-window formula and the run packet's "
            "already-declared prior evidence for this candidate on an 8 GB Windows host"
        ),
    }


def build_env_record(args, extra: dict | None = None) -> dict:
    env = {
        "framework": "nemo_toolkit (speechlm2.models.SALM)",
        "framework_version": None,
        "model_revision": PINNED_REVISION,
        "dtype": args.dtype,
        "device": args.device,
        "attn_impl": args.attn_impl,
        "decode_mode": "greedy",
        "beam_width": 1,
        "temperature": None,
        "lang_token": args.language,
        "task_token": "asr_transcribe",
        "punct_case_flag": "model_default",
        "max_input_s": MAX_TRAINED_CLIP_S,
        "max_new_tokens": args.max_new_tokens,
        "batch_size": 1,
        "vad": "none",
        "waveform_norm": "none",
        "chunk_size_s": CHUNK_S,
        "stride_s": STRIDE_S,
        "overlap_s": OVERLAP_S,
        "reassembly": REASSEMBLY,
        "timestamps": False,
        "prompt_context_bias": "none",
        "asr_mode_instruction": ASR_INSTRUCTION_PREFIX + "<audio_locator_tag>",
        "seed": args.seed,
        "non_default_params": {},
        "gpu": None,
        "vram_mb": 0,
        "ram": 0,
        "os": platform.platform(),
        "driver": None,
        "host_class": None,
        "license": LICENSE,
        "attribution": ATTRIBUTION,
        "hf_repo": HF_REPO,
    }
    if args.max_new_tokens != OFFICIAL_DEMO_MAX_NEW_TOKENS:
        env["non_default_params"]["max_new_tokens"] = {
            "value": args.max_new_tokens,
            "official_demo_default": OFFICIAL_DEMO_MAX_NEW_TOKENS,
            "reason": (
                "cli override; decode-length safety margin under the model's own "
                "1024-token sequence budget for a 40s window, no context/bias change"
            ),
        }
    if extra:
        env.update(extra)
    return env


def _load_model(args):
    """Heavy, network-safe local-only load. Imported lazily so --dry-run and
    py_compile never require nemo_toolkit/torch to be installed."""
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
    import torch  # noqa: F401 - imported for side effects / dtype access parity with other adapters
    from nemo.collections.speechlm2.models import SALM

    if not args.model_dir.is_dir():
        raise FileNotFoundError(
            f"--model-dir {args.model_dir} does not exist; this adapter never downloads "
            "weights itself -- run the acquisition lane first"
        )

    # Local-directory loading against NeMo's speechlm2 SALM.from_pretrained
    # is expected (see module docstring) to resolve entirely offline once
    # HF_HUB_OFFLINE/TRANSFORMERS_OFFLINE are set, mirroring the hub id
    # form shown in the official snippet -- this is not confirmed against a
    # live install since Canary-Qwen is deferred in this run.
    model = SALM.from_pretrained(str(args.model_dir))
    model = model.to(args.device).eval()
    return model


def _write_window_wav(waveform, sr: int, start_s: float, end_s: float, tmp_dir: Path) -> str:
    import soundfile as sf

    start_i = int(start_s * sr)
    end_i = int(end_s * sr)
    window = waveform[start_i:end_i]
    out_path = tmp_dir / f"window_{start_i:09d}_{end_i:09d}.wav"
    sf.write(str(out_path), window, sr)
    return str(out_path)


def _decode_one(model, args, wav_path: str, duration_s: float) -> tuple[str, str]:
    """Windowed official-path decode. Returns (hypothesis, status)."""
    import librosa

    waveform, _sr = librosa.load(wav_path, sr=SAMPLE_RATE)
    bounds = window_bounds(duration_s)
    transcripts = []
    any_truncated = False

    with tempfile.TemporaryDirectory(prefix="canary_qwen_bakeoff_") as tmp:
        tmp_dir = Path(tmp)
        for start_s, end_s in bounds:
            window_wav = _write_window_wav(waveform, SAMPLE_RATE, start_s, end_s, tmp_dir)
            prompts = [[
                {
                    "role": "user",
                    "content": f"{ASR_INSTRUCTION_PREFIX}{model.audio_locator_tag}",
                    "audio": [window_wav],
                }
            ]]
            answer_ids = model.generate(prompts=prompts, max_new_tokens=args.max_new_tokens)
            ids = answer_ids[0]
            ids = ids.cpu() if hasattr(ids, "cpu") else ids
            text = model.tokenizer.ids_to_text(ids).strip()
            token_count = len(ids) if hasattr(ids, "__len__") else None
            if token_count is not None and token_count >= args.max_new_tokens:
                any_truncated = True
            transcripts.append(text)

    # REASSEMBLY documents this policy in env.json; the join itself is the
    # policy (concatenate in chunk order, space-joined -- see module docstring).
    hypothesis = " ".join(t for t in transcripts if t)
    if not hypothesis:
        return "", "empty"
    if any_truncated:
        return hypothesis, "truncation"
    return hypothesis, "ok"


def run(args) -> int:
    rows = read_manifest(args.manifest)

    if args.dry_run:
        record = {
            "adapter_key": ADAPTER_KEY,
            "hf_repo": HF_REPO,
            "pinned_revision": PINNED_REVISION,
            "license": LICENSE,
            "attribution": ATTRIBUTION,
            "model_dir": str(args.model_dir),
            "model_dir_exists": args.model_dir.is_dir(),
            "manifest_rows": len(rows),
            "long_form_windowing": {
                "chunk_s": CHUNK_S,
                "stride_s": STRIDE_S,
                "overlap_s": OVERLAP_S,
                "reassembly": REASSEMBLY,
                "documented_in": "model card CasualConversations-v1 eval methodology (non-overlapping 40s chunks)",
            },
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
    model = _load_model(args)
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
    if selected:
        warmup_id = selected[0]["id"]
        try:
            _decode_one(model, args, selected[0]["path"], selected[0]["duration_s"])
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
                    hypothesis, row_status = _decode_one(model, args, row["path"], row["duration_s"])
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
