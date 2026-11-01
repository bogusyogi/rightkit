#!/usr/bin/env python3
"""Validate a cache-aware Windows ONNX encoder over three chained steps."""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

import numpy as np
import onnxruntime as ort


INPUT_NAMES = [
    "melspectrogram_features",
    "mel_len",
    "cache_last_channel",
    "cache_last_time",
    "cache_last_channel_len",
]
OUTPUT_NAMES = [
    "encoder_output_embeds",
    "encoder_output_length",
    "cache_last_channel_next",
    "cache_last_time_next",
    "cache_last_channel_len_next",
]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def describe(values) -> dict[str, object]:
    array = np.asarray(values)
    return {
        "shape": list(array.shape),
        "dtype": str(array.dtype),
        "finite": bool(np.isfinite(array).all()) if np.issubdtype(array.dtype, np.floating) else True,
        "abs_max": float(np.abs(array).max()) if array.size else 0.0,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--provider", default="CPUExecutionProvider")
    parser.add_argument("--skip-checker", action="store_true")
    args = parser.parse_args()

    onnx_version = None
    operations = None
    if not args.skip_checker:
        import onnx

        onnx.checker.check_model(str(args.model))
        model = onnx.load(str(args.model), load_external_data=False)
        operations = Counter(node.op_type for node in model.graph.node)
        onnx_version = onnx.__version__
    requested_providers = [args.provider]
    if args.provider != "CPUExecutionProvider":
        requested_providers.append("CPUExecutionProvider")
    session = ort.InferenceSession(str(args.model), providers=requested_providers)
    if [value.name for value in session.get_inputs()] != INPUT_NAMES:
        raise RuntimeError(f"input contract drifted: {[value.name for value in session.get_inputs()]}")
    if [value.name for value in session.get_outputs()] != OUTPUT_NAMES:
        raise RuntimeError(f"output contract drifted: {[value.name for value in session.get_outputs()]}")

    cache_channel = np.zeros((1, 24, 125, 1024), dtype=np.float32)
    cache_time = np.zeros((1, 24, 1024, 4), dtype=np.float32)
    cache_len = np.zeros((1,), dtype=np.int64)
    length = np.array([409], dtype=np.int64)
    steps = []
    for step in range(3):
        features = np.random.default_rng(20260720 + step).standard_normal(
            (1, 128, 409), dtype=np.float32
        )
        outputs = session.run(
            OUTPUT_NAMES,
            {
                INPUT_NAMES[0]: features,
                INPUT_NAMES[1]: length,
                INPUT_NAMES[2]: cache_channel,
                INPUT_NAMES[3]: cache_time,
                INPUT_NAMES[4]: cache_len,
            },
        )
        row = {name: describe(value) for name, value in zip(OUTPUT_NAMES, outputs)}
        if outputs[0].shape != (1, 1024, 25):
            raise RuntimeError(f"encoder output shape drifted at step {step}: {outputs[0].shape}")
        if outputs[2].shape != cache_channel.shape or outputs[3].shape != cache_time.shape:
            raise RuntimeError(f"cache shape drifted at step {step}")
        if not all(bool(row[name]["finite"]) for name in OUTPUT_NAMES):
            raise RuntimeError(f"non-finite output at step {step}: {row}")
        steps.append(row)
        cache_channel, cache_time, cache_len = outputs[2], outputs[3], outputs[4]

    receipt = {
        "status": "ok",
        "artifact": {
            "path": str(args.model),
            "bytes": args.model.stat().st_size,
            "sha256": sha256_file(args.model),
        },
        "versions": {"onnx": onnx_version, "onnxruntime": ort.__version__},
        "providers": session.get_providers(),
        "op_counts": dict(sorted(operations.items())) if operations is not None else None,
        "inputs": [
            {"name": value.name, "shape": value.shape, "type": value.type}
            for value in session.get_inputs()
        ],
        "outputs": [
            {"name": value.name, "shape": value.shape, "type": value.type}
            for value in session.get_outputs()
        ],
        "steps": steps,
    }
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps({key: value for key, value in receipt.items() if key != "steps"}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
