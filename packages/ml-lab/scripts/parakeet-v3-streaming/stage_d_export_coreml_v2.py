#!/usr/bin/env python3
"""Export the cache-aware encoder to Core ML through NeMo TorchScript export."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import traceback
from pathlib import Path

import numpy as np
import torch

from nemo_cache_export import configure_cache_export, export_with_nemo


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
RTOL = 1e-3
ATOL = 1e-4


def static_scalar_value(value):
    """Return the sole value from a scalar/singleton constant array."""

    array = np.asarray(value)
    if array.size != 1:
        raise ValueError(f"static cast value must contain exactly one element, got {array.shape}")
    return array.reshape(-1)[0].item()


def install_coreml_singleton_static_cast_patch() -> None:
    """Fix coremltools 9.0 casting a singleton constant ndarray with ``int``.

    Torch 2.11 represents fixed traced size values as rank-one singleton
    constants. Core ML already validates that this shape is scalar-equivalent,
    but its constant-fold branch calls ``int(ndarray)`` and raises. Scalarize
    only that validated compile-time case; every dynamic and true scalar path
    remains owned by coremltools.
    """

    from coremltools.converters.mil.frontend.torch import ops as torch_ops
    from coremltools.converters.mil.mil import Builder as mb

    original_cast = torch_ops._cast

    def singleton_safe_cast(context, node, dtype, dtype_name):
        inputs = torch_ops._get_inputs(context, node, expected=1)
        value = inputs[0]
        if (
            value.can_be_folded_to_const()
            and isinstance(value.val, np.ndarray)
            and value.val.ndim > 0
            and value.val.size == 1
        ):
            result = mb.const(val=dtype(static_scalar_value(value.val)), name=node.name)
            context.add(result, node.name)
            return
        original_cast(context, node, dtype, dtype_name)

    torch_ops._cast = singleton_safe_cast


def sha256_file(path: Path, chunk: int = 1 << 20) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(chunk), b""):
            digest.update(block)
    return digest.hexdigest()


def directory_digest(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    for file_path in sorted(item for item in path.rglob("*") if item.is_file()):
        relative = file_path.relative_to(path).as_posix()
        digest.update(relative.encode("utf-8"))
        with file_path.open("rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                total += len(block)
                digest.update(block)
    return digest.hexdigest(), total


def zero_cache_inputs(model, contract, seed: int):
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


def parity_rows(actual, expected) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for got, wanted in zip(actual, expected):
        diff = (got.detach().cpu().to(torch.float64) - wanted.detach().cpu().to(torch.float64)).abs()
        tolerance = ATOL + RTOL * wanted.detach().cpu().to(torch.float64).abs()
        rows.append(
            {
                "match": bool(torch.all(diff <= tolerance)),
                "max_abs_diff": float(diff.max()) if diff.numel() else 0.0,
                "elements_over_tolerance": int(torch.count_nonzero(diff > tolerance)),
                "elements": int(diff.numel()),
            }
        )
    return rows


def _spec_shape(feature) -> list[int]:
    return [int(value) for value in feature.type.multiArrayType.shape]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nemo", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--skip-palettize", action="store_true")
    parser.add_argument(
        "--reuse-torchscript",
        action="store_true",
        help="reuse the existing NeMo-exported TorchScript after re-running parity",
    )
    args = parser.parse_args()

    out = Path(args.out)
    torchscript_dir = out / "torchscript-fp32"
    macos_dir = out / "macos-p4"
    torchscript_dir.mkdir(parents=True, exist_ok=True)
    macos_dir.mkdir(parents=True, exist_ok=True)
    receipt: dict[str, object] = {
        "stage": "D",
        "status": "error",
        "export_path": "NeMo Exportable.export TorchScript with cache_support=True",
        "coreml_recipe": {
            "convert_to": "mlprogram",
            "compute_precision": "FLOAT16",
            "compute_units": "CPU_AND_NE",
            "minimum_deployment_target": "macOS14",
            "weight_precision": "4-bit kmeans palette",
            "granularity": "per_tensor",
            "per_channel_scale": False,
            "conv_weights": "fp16",
        },
    }

    try:
        import coremltools as ct
        import nemo
        import nemo.collections.asr as nemo_asr

        receipt["versions"] = {
            "coremltools": ct.__version__,
            "nemo": nemo.__version__,
            "torch": torch.__version__,
        }
        model = nemo_asr.models.ASRModel.restore_from(args.nemo, map_location="cpu").eval()
        contract = configure_cache_export(model)
        encoder = model.encoder
        example = zero_cache_inputs(model, contract, 20260720)
        with torch.no_grad():
            eager_outputs = encoder.forward_for_export(*example)
        if int(eager_outputs[0].shape[-1]) != contract.valid_out_len:
            raise RuntimeError(
                f"NeMo export output length drifted: {eager_outputs[0].shape[-1]} "
                f"!= {contract.valid_out_len}"
            )

        torchscript_path = torchscript_dir / "AudioEncoderCacheExplicit.pt"
        if args.reuse_torchscript:
            if not torchscript_path.is_file():
                raise RuntimeError(f"requested TorchScript resume artifact is missing: {torchscript_path}")
            receipt["torchscript_resumed"] = True
        else:
            export_with_nemo(encoder, torchscript_path, input_example=example)
            receipt["torchscript_resumed"] = False
        traced = torch.jit.load(str(torchscript_path), map_location="cpu").eval()
        with torch.no_grad():
            traced_outputs = traced(*example)
        trace_parity = parity_rows(traced_outputs, eager_outputs)
        if not all(bool(row["match"]) for row in trace_parity):
            raise RuntimeError(f"TorchScript parity failed: {trace_parity}")

        int_ops = sum(
            1
            for node in traced.inlined_graph.nodes()
            if node.kind() in {"aten::Int", "aten::item"}
        )
        receipt["contract"] = contract.to_dict()
        receipt["example_shapes"] = {
            "inputs": [list(tensor.shape) for tensor in example],
            "outputs": [list(tensor.shape) for tensor in eager_outputs],
        }
        receipt["torchscript"] = {
            "path": str(torchscript_path.relative_to(out)).replace(os.sep, "/"),
            "bytes": torchscript_path.stat().st_size,
            "sha256": sha256_file(torchscript_path),
            "eager_parity": trace_parity,
            "rtol": RTOL,
            "atol": ATOL,
            "aten_int_or_item_ops": int_ops,
        }

        ct_inputs = [
            ct.TensorType(name=INPUT_NAMES[0], shape=tuple(example[0].shape), dtype=np.float16),
            ct.TensorType(name=INPUT_NAMES[1], shape=tuple(example[1].shape), dtype=np.int32),
            ct.TensorType(name=INPUT_NAMES[2], shape=tuple(example[2].shape), dtype=np.float16),
            ct.TensorType(name=INPUT_NAMES[3], shape=tuple(example[3].shape), dtype=np.float16),
            ct.TensorType(name=INPUT_NAMES[4], shape=tuple(example[4].shape), dtype=np.int32),
        ]
        ct_outputs = [
            ct.TensorType(name=OUTPUT_NAMES[0], dtype=np.float16),
            ct.TensorType(name=OUTPUT_NAMES[1], dtype=np.int32),
            ct.TensorType(name=OUTPUT_NAMES[2], dtype=np.float16),
            ct.TensorType(name=OUTPUT_NAMES[3], dtype=np.float16),
            ct.TensorType(name=OUTPUT_NAMES[4], dtype=np.int32),
        ]
        install_coreml_singleton_static_cast_patch()
        receipt["coremltools_frontend_compatibility"] = {
            "applied": True,
            "scope": "singleton compile-time ndarray passed to aten::Int",
            "graph_or_weight_changes": False,
        }
        mlmodel = ct.convert(
            traced,
            inputs=ct_inputs,
            outputs=ct_outputs,
            convert_to="mlprogram",
            compute_precision=ct.precision.FLOAT16,
            compute_units=ct.ComputeUnit.CPU_AND_NE,
            minimum_deployment_target=ct.target.macOS14,
        )

        if args.skip_palettize:
            receipt["palettization"] = {"applied": False, "reason": "--skip-palettize"}
        else:
            from coremltools.optimize.coreml import (
                OpPalettizerConfig,
                OptimizationConfig,
                palettize_weights,
            )

            palette = OptimizationConfig(
                global_config=OpPalettizerConfig(
                    nbits=4,
                    mode="kmeans",
                    granularity="per_tensor",
                    enable_per_channel_scale=False,
                ),
                op_type_configs={"conv": None},
            )
            mlmodel = palettize_weights(mlmodel, config=palette)
            receipt["palettization"] = {
                "applied": True,
                "nbits": 4,
                "mode": "kmeans",
                "granularity": "per_tensor",
                "per_channel_scale": False,
                "conv_kept_fp16": True,
            }

        package = macos_dir / "AudioEncoderCacheExplicit.mlpackage"
        if package.exists():
            shutil.rmtree(package)
        mlmodel.save(str(package))
        spec = mlmodel.get_spec()
        spec_inputs = [
            {
                "name": feature.name,
                "shape": _spec_shape(feature),
                "dtype": int(feature.type.multiArrayType.dataType),
            }
            for feature in spec.description.input
        ]
        spec_outputs = [
            {
                "name": feature.name,
                "shape": _spec_shape(feature),
                "dtype": int(feature.type.multiArrayType.dataType),
            }
            for feature in spec.description.output
        ]
        if [item["name"] for item in spec_inputs] != INPUT_NAMES:
            raise RuntimeError(f"Core ML input names drifted: {spec_inputs}")
        if [item["name"] for item in spec_outputs] != OUTPUT_NAMES:
            raise RuntimeError(f"Core ML output names drifted: {spec_outputs}")
        if spec_outputs[0]["shape"][-1] != contract.valid_out_len:
            raise RuntimeError(f"Core ML output shape drifted: {spec_outputs[0]}")
        package_hash, package_bytes = directory_digest(package)
        receipt["io_contract"] = {"inputs": spec_inputs, "outputs": spec_outputs}
        receipt["has_mlstate"] = len(spec.description.state) > 0
        receipt["artifact"] = {
            "path": str(package.relative_to(out)).replace(os.sep, "/"),
            "bytes": package_bytes,
            "sha256_recursive": package_hash,
            "specification_version": int(spec.specificationVersion),
        }
        receipt["status"] = "ok"
    except Exception as error:
        receipt["stop_reason"] = f"{type(error).__name__}: {error}"
        receipt["traceback"] = traceback.format_exc(limit=18)

    receipt_path = out / "stage-d-receipt.json"
    receipt_path.write_text(
        json.dumps(receipt, indent=2, sort_keys=True, default=str), encoding="utf-8"
    )
    print(json.dumps(receipt, indent=2, default=str))
    return 0 if receipt["status"] == "ok" else 2


if __name__ == "__main__":
    raise SystemExit(main())
