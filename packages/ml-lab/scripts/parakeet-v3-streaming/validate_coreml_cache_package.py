#!/usr/bin/env python3
"""Independently validate the saved cache-aware Core ML package."""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

import coremltools as ct


EXPECTED_INPUTS = [
    "melspectrogram_features",
    "mel_len",
    "cache_last_channel",
    "cache_last_time",
    "cache_last_channel_len",
]
EXPECTED_OUTPUTS = [
    "encoder_output_embeds",
    "encoder_output_length",
    "cache_last_channel_next",
    "cache_last_time_next",
    "cache_last_channel_len_next",
]


def directory_digest(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    for file_path in sorted(item for item in path.rglob("*") if item.is_file()):
        digest.update(file_path.relative_to(path).as_posix().encode("utf-8"))
        with file_path.open("rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                total += len(block)
                digest.update(block)
    return digest.hexdigest(), total


def shape(feature) -> list[int]:
    return [int(value) for value in feature.type.multiArrayType.shape]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()

    spec = ct.utils.load_spec(str(args.package))
    if spec.WhichOneof("Type") != "mlProgram":
        raise RuntimeError(f"expected mlProgram, got {spec.WhichOneof('Type')}")
    inputs = list(spec.description.input)
    outputs = list(spec.description.output)
    if [item.name for item in inputs] != EXPECTED_INPUTS:
        raise RuntimeError(f"input names drifted: {[item.name for item in inputs]}")
    if [item.name for item in outputs] != EXPECTED_OUTPUTS:
        raise RuntimeError(f"output names drifted: {[item.name for item in outputs]}")
    if shape(outputs[0]) != [1, 1024, 25]:
        raise RuntimeError(f"encoder output shape drifted: {shape(outputs[0])}")

    function = spec.mlProgram.functions["main"]
    block = function.block_specializations[function.opset]
    operation_counts = Counter(operation.type for operation in block.operations)
    producer = {
        output.name: operation.type
        for operation in block.operations
        for output in operation.outputs
    }
    conv_weight_producers = Counter()
    for operation in block.operations:
        if operation.type != "conv":
            continue
        for argument in operation.inputs["weight"].arguments:
            conv_weight_producers[producer.get(argument.name, "input_or_unknown")] += 1
    if operation_counts["constexpr_lut_to_dense"] != 217:
        raise RuntimeError(
            f"expected 217 palettized linear weights, got {operation_counts['constexpr_lut_to_dense']}"
        )
    if conv_weight_producers != {"const": 77}:
        raise RuntimeError(f"convolution precision drifted: {dict(conv_weight_producers)}")

    digest, total = directory_digest(args.package)
    receipt = {
        "status": "ok",
        "coremltools": ct.__version__,
        "artifact": {
            "path": str(args.package),
            "bytes": total,
            "sha256_recursive": digest,
        },
        "specification_version": int(spec.specificationVersion),
        "program_opset": function.opset,
        "inputs": [
            {"name": item.name, "shape": shape(item), "dtype": int(item.type.multiArrayType.dataType)}
            for item in inputs
        ],
        "outputs": [
            {"name": item.name, "shape": shape(item), "dtype": int(item.type.multiArrayType.dataType)}
            for item in outputs
        ],
        "operation_counts": dict(sorted(operation_counts.items())),
        "conv_weight_producers": dict(conv_weight_producers),
        "has_mlstate": len(spec.description.state) > 0,
    }
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
