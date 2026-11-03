#!/usr/bin/env python3
"""Export the cache-aware encoder through NeMo's supported ONNX path.

The pinned model is configured with ``cache_support=True`` and exported through
``encoder.export``. That API temporarily routes ``forward`` to
``forward_for_export`` and owns the batch-first cache boundary contract.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import traceback
from pathlib import Path

import numpy as np
import torch

from nemo_cache_export import (
    ONNX_OPSET,
    configure_cache_export,
    export_with_nemo,
)


RTOL = 1e-3
ATOL = 1e-4
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


def sha256_file(path: Path, chunk: int = 1 << 20) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(chunk), b""):
            digest.update(block)
    return digest.hexdigest()


def _rename_graph_io(model, input_names: list[str], output_names: list[str]) -> None:
    graph = model.graph
    if len(graph.input) != len(input_names):
        raise RuntimeError(
            f"NeMo ONNX input count drifted: {len(graph.input)} != {len(input_names)}; "
            f"got {[item.name for item in graph.input]}"
        )
    if len(graph.output) != len(output_names):
        raise RuntimeError(
            f"NeMo ONNX output count drifted: {len(graph.output)} != {len(output_names)}; "
            f"got {[item.name for item in graph.output]}"
        )
    mapping = {
        **{item.name: name for item, name in zip(graph.input, input_names)},
        **{item.name: name for item, name in zip(graph.output, output_names)},
    }
    for item in list(graph.input) + list(graph.output) + list(graph.value_info):
        item.name = mapping.get(item.name, item.name)
    for node in graph.node:
        for index, name in enumerate(node.input):
            node.input[index] = mapping.get(name, name)
        for index, name in enumerate(node.output):
            node.output[index] = mapping.get(name, name)


def _consolidate_and_name_onnx(path: Path) -> Path:
    import onnx

    data_name = f"{path.name}.data"
    model = onnx.load(str(path), load_external_data=True)
    _rename_graph_io(model, INPUT_NAMES, OUTPUT_NAMES)
    for sibling in path.parent.iterdir():
        if sibling != path and sibling.is_file():
            sibling.unlink()
    onnx.save_model(
        model,
        str(path),
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location=data_name,
        size_threshold=1024,
        convert_attribute=False,
    )
    del model
    onnx.checker.check_model(str(path))
    return path.parent / data_name


def _close(actual, expected) -> dict[str, object]:
    actual = np.asarray(actual)
    expected = np.asarray(expected)
    if actual.shape != expected.shape:
        return {
            "match": False,
            "actual_shape": list(actual.shape),
            "expected_shape": list(expected.shape),
        }
    diff = np.abs(actual.astype(np.float64) - expected.astype(np.float64))
    tolerance = ATOL + RTOL * np.abs(expected.astype(np.float64))
    return {
        "match": bool(np.all(diff <= tolerance)),
        "max_abs_diff": float(diff.max()) if diff.size else 0.0,
        "elements_over_tolerance": int(np.count_nonzero(diff > tolerance)),
        "elements": int(diff.size),
    }


def _zero_cache_export_inputs(model, contract, seed: int):
    encoder = model.encoder
    mel_bins = int(model.preprocessor._cfg["features"])
    generator = torch.Generator().manual_seed(seed)
    features = torch.randn(
        1,
        mel_bins,
        contract.input_feature_frames,
        generator=generator,
        dtype=torch.float32,
    )
    length = torch.tensor([contract.input_feature_frames], dtype=torch.int64)
    channel, time, channel_len = encoder.get_initial_cache_state(batch_size=1)
    return (
        features,
        length,
        channel.transpose(0, 1).contiguous(),
        time.transpose(0, 1).contiguous(),
        channel_len,
    )


def _run_parity(model, onnx_path: Path, contract) -> dict[str, object]:
    import onnxruntime as ort

    encoder = model.encoder
    session = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])

    first = _zero_cache_export_inputs(model, contract, 20260720)
    second_features = _zero_cache_export_inputs(model, contract, 20260721)[0]

    def eager(inputs):
        with torch.no_grad():
            return encoder.forward_for_export(*inputs)

    def exported(inputs):
        return session.run(
            OUTPUT_NAMES,
            {name: tensor.detach().cpu().numpy() for name, tensor in zip(INPUT_NAMES, inputs)},
        )

    reference0 = eager(first)
    actual0 = exported(first)
    reference1_inputs = (second_features, first[1], reference0[2], reference0[3], reference0[4])
    actual1_inputs = (
        second_features,
        first[1],
        torch.from_numpy(actual0[2]),
        torch.from_numpy(actual0[3]),
        torch.from_numpy(actual0[4]),
    )
    reference1 = eager(reference1_inputs)
    actual1 = exported(actual1_inputs)

    detail: dict[str, object] = {}
    for step, actual, expected in (
        (0, actual0, reference0),
        (1, actual1, reference1),
    ):
        for name, got, wanted in zip(OUTPUT_NAMES, actual, expected):
            detail[f"step{step}.{name}"] = _close(got, wanted.detach().cpu().numpy())

    cache_bounded = all(
        np.asarray(actual0[index]).shape == np.asarray(actual1[index]).shape
        for index in (2, 3, 4)
    )
    reset = exported(first)
    reset_equal = all(np.array_equal(a, b) for a, b in zip(actual0, reset))
    all_close = all(bool(row["match"]) for row in detail.values())
    return {
        "rtol": RTOL,
        "atol": ATOL,
        "all_tensors_match": all_close,
        "cache_shapes_bounded": cache_bounded,
        "reset_is_bit_exact": reset_equal,
        "detail": detail,
        "session_inputs": [
            {"name": value.name, "shape": value.shape, "type": value.type}
            for value in session.get_inputs()
        ],
        "session_outputs": [
            {"name": value.name, "shape": value.shape, "type": value.type}
            for value in session.get_outputs()
        ],
    }


def _write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True, default=str), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nemo", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--opset", type=int, default=ONNX_OPSET)
    args = parser.parse_args()

    out = Path(args.out)
    onnx_dir = out / "onnx-fp32"
    onnx_dir.mkdir(parents=True, exist_ok=True)
    receipt: dict[str, object] = {
        "stage": "C",
        "status": "error",
        "export_path": "NeMo Exportable.export with cache_support=True",
    }

    try:
        import nemo
        import nemo.collections.asr as nemo_asr
        import onnx
        import onnxruntime

        model = nemo_asr.models.ASRModel.restore_from(args.nemo, map_location="cpu").eval()
        contract = configure_cache_export(model)
        encoder = model.encoder
        receipt["versions"] = {
            "nemo": nemo.__version__,
            "torch": torch.__version__,
            "onnx": onnx.__version__,
            "onnxruntime": onnxruntime.__version__,
        }
        receipt["nemo_declared_io"] = {
            "inputs": list(encoder.input_names),
            "outputs": list(encoder.output_names),
        }

        example = _zero_cache_export_inputs(model, contract, 20260720)
        with torch.no_grad():
            example_outputs = encoder.forward_for_export(*example)
        receipt["example_shapes"] = {
            "inputs": [list(tensor.shape) for tensor in example],
            "outputs": [list(tensor.shape) for tensor in example_outputs],
        }
        if int(example_outputs[0].shape[-1]) != contract.valid_out_len:
            raise RuntimeError(
                f"NeMo export output length drifted: {example_outputs[0].shape[-1]} "
                f"!= {contract.valid_out_len}"
            )

        onnx_path = onnx_dir / "AudioEncoderCacheExplicit.onnx"
        export_with_nemo(
            encoder,
            onnx_path,
            input_example=example,
            onnx_opset_version=args.opset,
        )
        data_path = _consolidate_and_name_onnx(onnx_path)
        parity = _run_parity(model, onnx_path, contract)

        receipt["contract"] = contract.to_dict()
        receipt["parity"] = parity
        receipt["artifact"] = {
            "graph": {
                "path": str(onnx_path.relative_to(out)).replace(os.sep, "/"),
                "bytes": onnx_path.stat().st_size,
                "sha256": sha256_file(onnx_path),
            },
            "external_data": {
                "path": str(data_path.relative_to(out)).replace(os.sep, "/"),
                "bytes": data_path.stat().st_size,
                "sha256": sha256_file(data_path),
            },
            "opset": args.opset,
        }
        receipt["status"] = (
            "ok"
            if parity["all_tensors_match"]
            and parity["cache_shapes_bounded"]
            and parity["reset_is_bit_exact"]
            else "gate_failed"
        )
        if receipt["status"] != "ok":
            receipt["stop_reason"] = "one or more exact ONNX parity gates failed"
    except Exception as error:
        receipt["stop_reason"] = f"{type(error).__name__}: {error}"
        receipt["traceback"] = traceback.format_exc(limit=16)

    _write_json(out / "cache-contract.json", receipt.get("contract", {}))
    _write_json(out / "stage-c-receipt.json", receipt)
    print(json.dumps({key: value for key, value in receipt.items() if key != "parity"}, indent=2, default=str))
    return 0 if receipt["status"] == "ok" else 2


if __name__ == "__main__":
    raise SystemExit(main())
