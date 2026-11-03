"""Cohere Transcribe ONNX q4f16 — the actual deployable INT4 artifact.

The bitsandbytes nf4 run in `cohere_quant_smoke.py` proves 4-bit *quality* but not
4-bit *speed*: bnb is compute-inefficient, while these ONNX exports quantize the
weights to 4 bits and run through MatMulNBits, which has real CUDA kernels. This
script runs the artifact itself so both numbers are measured rather than inferred.

Artifact (`onnx-community/cohere-transcribe-03-2026-ONNX`):
    encoder_model_q4f16.onnx_data          1369 MB
    decoder_model_merged_q4f16.onnx_data     93 MB   -> ~1.46 GB total

Feature extraction and tokenization come from the reference bf16 checkpoint's
processor, so the only thing under test is the quantized graph.

Decode is greedy, matching `generate(..., do_sample=False)`. The decoder prompt is
whatever the processor emits for `language="en"` — 10 tokens covering
startofcontext / startoftranscript / emotion / language / pnc / itn / timestamp /
diarize — not hand-rolled, so it cannot drift from the reference call.

    .venv-infer/Scripts/python.exe cohere_onnx_q4_smoke.py \
        --canonical-dir canonical-eval --limit 20
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import time
from pathlib import Path

import numpy as np

DEFAULT_ONNX_DIR = Path("D:/Claude/heardright/.cache/asr-models/cohere-q4f16")
REFERENCE_DIR = Path("D:/Claude/heardright/.cache/asr-models/cohere-transcribe")
N_LAYERS = 8
N_HEADS = 8
HEAD_DIM = 128


def load_helpers():
    here = Path(__file__).parent
    spec = importlib.util.spec_from_file_location(
        "verifier", here / "productivity-corpus" / "verify_tts_intelligibility.py"
    )
    verifier = importlib.util.module_from_spec(spec)
    sys.modules["verifier"] = verifier
    spec.loader.exec_module(verifier)

    source = (here / "cohere_quant_smoke.py").read_text(encoding="utf-8")
    ns: dict = {}
    exec(compile(source.split("def build_model")[0], "quant", "exec"), ns)
    return verifier, ns["load_subset"]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--canonical-dir", required=True, type=Path)
    parser.add_argument("--onnx-dir", type=Path, default=DEFAULT_ONNX_DIR)
    parser.add_argument("--reference-dir", type=Path, default=REFERENCE_DIR)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument("--provider", default="CUDAExecutionProvider")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    # ORT's CUDA EP needs CUDA/cuDNN DLLs on the search path. Torch ships them in
    # site-packages/nvidia/*/bin; register those before importing onnxruntime or
    # the EP silently falls back to CPU with only a warning.
    import glob
    import os

    import torch

    nvidia_root = Path(torch.__file__).parent.parent / "nvidia"
    for dll_dir in glob.glob(str(nvidia_root / "*" / "bin")):
        if os.path.isdir(dll_dir):
            os.add_dll_directory(os.path.abspath(dll_dir))

    import onnxruntime as ort
    from transformers import AutoProcessor
    from transformers.audio_utils import load_audio

    verifier, load_subset = load_helpers()
    rows = load_subset(args.canonical_dir, args.limit)
    audio_s = sum(r["duration_s"] for r in rows)
    print(f"subset: {len(rows)} clips, {audio_s/60:.1f} min audio", flush=True)

    processor = AutoProcessor.from_pretrained(str(args.reference_dir))
    eos_id = 3

    options = ort.SessionOptions()
    options.log_severity_level = 3
    providers = [args.provider, "CPUExecutionProvider"]
    load_start = time.time()
    encoder = ort.InferenceSession(
        str(args.onnx_dir / "onnx" / "encoder_model_q4f16.onnx"), options, providers=providers
    )
    decoder = ort.InferenceSession(
        str(args.onnx_dir / "onnx" / "decoder_model_merged_q4f16.onnx"),
        options,
        providers=providers,
    )
    load_s = time.time() - load_start
    active = encoder.get_providers()[0]
    print(f"loaded in {load_s:.1f}s on {active}", flush=True)
    if args.provider != "CPUExecutionProvider" and active != args.provider:
        raise SystemExit(
            f"{args.provider} unavailable — ORT fell back to {active}. A CPU number "
            "is not the GPU-tier answer, so refusing to report it as one."
        )

    total_err = 0
    total_ref = 0
    decode_s = 0.0
    per_group: dict[str, list[int]] = {}

    for row in rows:
        audio = load_audio(row["path"], sampling_rate=16000)
        features = processor(
            audio, sampling_rate=16000, return_tensors="np", language="en"
        )
        input_features = features["input_features"].astype(np.float32)
        prompt = np.asarray(features["decoder_input_ids"], dtype=np.int64).reshape(1, -1)

        start = time.time()
        # Audio longer than the feature extractor's window comes back as several
        # chunks in dim 0. Each chunk is its own decode; the processor's own
        # long-form path reassembles them in order via audio_chunk_index, so
        # concatenating the per-chunk text matches that behaviour.
        all_features = encoder.run(
            ["last_hidden_state"], {"input_features": input_features}
        )[0]
        chunk_texts: list[str] = []
        for chunk_index in range(all_features.shape[0]):
            encoder_hidden = all_features[chunk_index : chunk_index + 1]

        # Greedy decode. First pass consumes the whole prompt with empty cache;
        # every later pass feeds one token plus the returned cache.
            # Mixed precision in this export: encoder graph and
            # encoder_hidden_states are fp32, but the KV cache tensors are fp16.
            # Inheriting one dtype for both is an INVALID_ARGUMENT.
            past = {
                f"past_key_values.{layer}.{side}.{kind}": np.zeros(
                    (1, N_HEADS, 0, HEAD_DIM), dtype=np.float16
                )
                for layer in range(N_LAYERS)
                for side in ("decoder", "encoder")
                for kind in ("key", "value")
            }
            tokens = prompt
            generated: list[int] = []
            budget = max(64, int(row["duration_s"] * 6) + 64)
            position = 0
            while len(generated) < budget:
                seq_len = tokens.shape[1]
                feeds = {
                    "input_ids": tokens,
                    "attention_mask": np.ones((1, position + seq_len), dtype=np.int64),
                    "position_ids": np.arange(
                        position, position + seq_len, dtype=np.int64
                    ).reshape(1, -1),
                    "num_logits_to_keep": np.array(1, dtype=np.int64),
                    "encoder_hidden_states": encoder_hidden,
                    **past,
                }
                outputs = decoder.run(None, feeds)
                logits = outputs[0]
                names = [o.name for o in decoder.get_outputs()]
                past = {
                    name.replace("present", "past_key_values"): value
                    for name, value in zip(names[1:], outputs[1:])
                }
                next_id = int(np.argmax(logits[0, -1]))
                if next_id == eos_id:
                    break
                generated.append(next_id)
                position += seq_len
                tokens = np.array([[next_id]], dtype=np.int64)
            chunk_texts.append(
                processor.tokenizer.decode(generated, skip_special_tokens=True)
            )

        decode_s += time.time() - start
        text = " ".join(t.strip() for t in chunk_texts if t.strip())
        reference = verifier.normalize(row["ground_truth"])
        rate, _, _, _ = verifier.wer(reference, verifier.normalize(text))
        errors = round(rate * len(reference))
        total_err += errors
        total_ref += len(reference)
        per_group.setdefault(row["group"], [0, 0])
        per_group[row["group"]][0] += errors
        per_group[row["group"]][1] += len(reference)
        print(
            f"  {row['id']} {row['duration_s']:6.1f}s WER={rate:6.2%}", flush=True
        )

    wer = total_err / max(1, total_ref)
    result = {
        "wer": round(wer, 4),
        "rtfx": round(audio_s / decode_s, 2),
        "load_s": round(load_s, 1),
        "provider": encoder.get_providers()[0],
        "by_group": {
            g: round(e / max(1, n), 4) for g, (e, n) in sorted(per_group.items())
        },
    }
    print(f"\nONNX q4f16  WER={wer:.2%}  RTFx={audio_s/decode_s:.2f}")
    for group, value in result["by_group"].items():
        print(f"   {group:<28} {value:.2%}")
    if args.out:
        args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
