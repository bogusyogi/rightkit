#!/usr/bin/env python3
"""Stage D — convert the cache-aware encoder to Core ML with explicit cache I/O.

coremltools dropped ONNX ingestion at v6, so this converts from the traced
PyTorch module directly (same CacheEncoder contract Stage C exported), not from
the Stage C .onnx. The ONNX artifact remains the numerical reference.

Produces AudioEncoderCacheExplicit.mlpackage:
  - ML Program, FLOAT16 compute precision, CPU_AND_NE, minimum target macOS14
  - batch 1, fixed dims from the Stage A / Stage C contract
  - encoder weights palettized to 4-bit k-means, per-tensor, no per-channel scale
  - convolution weights deliberately retained at fp16 (matches the locked p4
    precision family; palettizing depthwise conv is where p4 accuracy degrades)
  - no decoder/joint re-export, no MLState artifact

Core ML cannot execute under WSL. This stage does conversion plus structural
verification only; numerical acceptance happens on the Mac under CPU_AND_NE.

Usage:
  python stage_d_export_coreml.py --nemo <path> --out <dir> [--skip-palettize]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import traceback

import numpy as np
import torch
import torch.nn as nn

CHUNK_SIZE_FRAMES = 50
SHIFT_SIZE_FRAMES = 25
ATT_CONTEXT_SIZE = [125, 25]

IN_NAMES = [
    "melspectrogram_features",
    "mel_len",
    "cache_last_channel",
    "cache_last_time",
    "cache_last_channel_len",
]
OUT_NAMES = [
    "encoder_output_embeds",
    "encoder_output_length",
    "cache_last_channel_next",
    "cache_last_time_next",
    "cache_last_channel_len_next",
]


class CacheEncoder(nn.Module):
    """Explicit-cache wrapper: 5 inputs -> 5 outputs, no hidden state.

    const_lengths pins the two length tensors to their fixed-shape values instead
    of threading the traced inputs through the encoder. That is semantically
    correct for a fixed-dimension export -- every step feeds exactly
    chunk_mel_frames (zero-padded on the final chunk, trimmed host-side, the same
    contract the shipping fixed-window p4 encoder uses) and the cache is always
    allocated at full size. It exists because threading the traced length tensors
    produces per-layer aten::Int casts that coremltools cannot convert.
    """

    def __init__(self, encoder, const_lengths=None):
        super().__init__()
        self.encoder = encoder
        self.const_lengths = const_lengths
        if const_lengths is not None:
            self.register_buffer("_mel_len", const_lengths[0].clone())
            self.register_buffer("_cache_len", const_lengths[1].clone())

    def forward(
        self,
        melspectrogram_features,
        mel_len,
        cache_last_channel,
        cache_last_time,
        cache_last_channel_len,
    ):
        if self.const_lengths is not None:
            mel_len = self._mel_len
            cache_last_channel_len = self._cache_len
        out = self.encoder(
            audio_signal=melspectrogram_features,
            length=mel_len,
            cache_last_channel=cache_last_channel,
            cache_last_time=cache_last_time,
            cache_last_channel_len=cache_last_channel_len,
        )
        return out[0], out[1], out[2], out[3], out[4]


def dir_digest(path):
    """Deterministic SHA-256 over an .mlpackage directory, plus its byte size."""
    h = hashlib.sha256()
    total = 0
    for root, dirs, files in os.walk(path):
        dirs.sort()
        for name in sorted(files):
            fp = os.path.join(root, name)
            rel = os.path.relpath(fp, path).replace(os.sep, "/")
            h.update(rel.encode())
            with open(fp, "rb") as fh:
                for blk in iter(lambda: fh.read(1 << 20), b""):
                    h.update(blk)
                    total += len(blk)
    return h.hexdigest(), total


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--nemo", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--skip-palettize", action="store_true")
    ap.add_argument(
        "--const-lengths",
        action="store_true",
        help="pin mel_len and cache_last_channel_len to fixed-shape constants",
    )
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    receipt = {"stage": "D", "status": "error", "stop_reason": None}

    import coremltools as ct
    import nemo.collections.asr as nemo_asr

    receipt["coremltools"] = ct.__version__

    model = nemo_asr.models.ASRModel.restore_from(args.nemo, map_location="cpu")
    model.eval()
    enc = model.encoder
    enc.setup_streaming_params(
        chunk_size=CHUNK_SIZE_FRAMES,
        shift_size=SHIFT_SIZE_FRAMES,
        left_chunks=None,
        att_context_size=ATT_CONTEXT_SIZE,
    )
    scfg = enc.streaming_cfg
    mel_bins = int(model.preprocessor._cfg["features"])
    cs = scfg.chunk_size
    chunk = int(cs[1] if isinstance(cs, (list, tuple)) and len(cs) > 1 else cs)

    cache = enc.get_initial_cache_state(batch_size=1)
    const_lengths = None
    if args.const_lengths:
        const_lengths = (
            torch.tensor([chunk], dtype=torch.int64),
            torch.tensor(
                [int(getattr(scfg, "last_channel_cache_size", 125))], dtype=torch.int64
            ),
        )
    wrapper = CacheEncoder(enc, const_lengths=const_lengths).eval()
    receipt["const_lengths"] = (
        {"mel_len": chunk, "cache_last_channel_len": int(const_lengths[1][0])}
        if const_lengths
        else None
    )

    g = torch.Generator().manual_seed(20260720)
    example = (
        torch.randn(1, mel_bins, chunk, generator=g, dtype=torch.float32),
        torch.tensor([chunk], dtype=torch.int64),
        cache[0].clone(),
        cache[1].clone(),
        cache[2].clone(),
    )

    receipt["fixed_dims"] = {
        "melspectrogram_features": [1, mel_bins, chunk],
        "mel_len": [1],
        "cache_last_channel": list(cache[0].shape),
        "cache_last_time": list(cache[1].shape),
        "cache_last_channel_len": list(cache[2].shape),
    }

    # TorchScript tracing leaves 51 aten::Int ops (per-layer cache-length
    # threading) that coremltools rejects with "only 0-dimensional arrays can be
    # converted to Python scalars"; torch.jit.freeze does not fold them. The
    # torch.export path carries these as symbolic ints instead, so try it first
    # and keep tracing as the fallback.
    frontends = []
    try:
        with torch.no_grad():
            ep = torch.export.export(wrapper, example, strict=False)
        # coremltools accepts only ATEN/EDGE dialect; a freshly exported program
        # is TRAINING dialect. Decomposing lowers it and expands the composite
        # ops that carry the problematic tensor->int casts.
        ep = ep.run_decompositions({})
        frontends.append(("torch_export", ep))
    except Exception as e:
        receipt.setdefault("frontend_errors", {})["torch_export"] = (
            f"{type(e).__name__}: {e}"
        )
    try:
        with torch.no_grad():
            traced = torch.jit.trace(wrapper, example, strict=False)
        frontends.append(("torchscript_trace", traced))
    except Exception as e:
        receipt.setdefault("frontend_errors", {})["torchscript_trace"] = (
            f"{type(e).__name__}: {e}"
        )

    if not frontends:
        receipt["stop_reason"] = "both torch.export and torch.jit.trace failed"
        receipt["traceback"] = traceback.format_exc(limit=10)
        write(args.out, receipt)
        print(f"STOP: {receipt['stop_reason']}")
        return 0

    ct_inputs = [
        ct.TensorType(name=IN_NAMES[0], shape=example[0].shape, dtype=np.float32),
        ct.TensorType(name=IN_NAMES[1], shape=example[1].shape, dtype=np.int32),
        ct.TensorType(name=IN_NAMES[2], shape=example[2].shape, dtype=np.float32),
        ct.TensorType(name=IN_NAMES[3], shape=example[3].shape, dtype=np.float32),
        ct.TensorType(name=IN_NAMES[4], shape=example[4].shape, dtype=np.int32),
    ]

    mlmodel = None
    convert_errors = {}
    for label, prog in frontends:
        try:
            print(f"[stage D] converting via {label} ...")
            mlmodel = ct.convert(
                prog,
                inputs=ct_inputs,
                outputs=[ct.TensorType(name=n) for n in OUT_NAMES],
                convert_to="mlprogram",
                compute_precision=ct.precision.FLOAT16,
                compute_units=ct.ComputeUnit.CPU_AND_NE,
                minimum_deployment_target=ct.target.macOS14,
            )
            receipt["frontend_used"] = label
            break
        except Exception as e:
            convert_errors[label] = f"{type(e).__name__}: {e}"
            print(f"[stage D] {label} failed: {type(e).__name__}: {e}")

    if mlmodel is None:
        receipt["convert_errors"] = convert_errors
        receipt["stop_reason"] = "coreml convert failed on every frontend"
        receipt["traceback"] = traceback.format_exc(limit=12)
        write(args.out, receipt)
        print(f"STOP: {receipt['stop_reason']}")
        return 0

    receipt["converted"] = True
    if convert_errors:
        receipt["convert_errors"] = convert_errors

    if not args.skip_palettize:
        try:
            from coremltools.optimize.coreml import (
                OpPalettizerConfig,
                OptimizationConfig,
                palettize_weights,
            )

            cfg = OptimizationConfig(
                global_config=OpPalettizerConfig(
                    nbits=4, mode="kmeans", granularity="per_tensor"
                ),
                # Depthwise/pointwise conv stays fp16 — matches the locked p4 family.
                op_type_configs={"conv": None},
            )
            mlmodel = palettize_weights(mlmodel, config=cfg)
            receipt["palettization"] = {
                "applied": True,
                "nbits": 4,
                "mode": "kmeans",
                "granularity": "per_tensor",
                "per_channel_scale": False,
                "conv_kept_fp16": True,
            }
        except Exception as e:
            receipt["palettization"] = {
                "applied": False,
                "error": f"{type(e).__name__}: {e}",
            }
            receipt["stop_reason"] = "palettization failed"
            receipt["traceback"] = traceback.format_exc(limit=10)
            write(args.out, receipt)
            print(f"STOP: {receipt['stop_reason']}")
            return 0
    else:
        receipt["palettization"] = {"applied": False, "reason": "--skip-palettize"}

    pkg = os.path.join(args.out, "AudioEncoderCacheExplicit.mlpackage")
    if os.path.exists(pkg):
        shutil.rmtree(pkg)
    mlmodel.save(pkg)

    spec = mlmodel.get_spec()
    receipt["io_contract"] = {
        "inputs": [
            {"name": i.name, "type": i.type.WhichOneof("Type")} for i in spec.description.input
        ],
        "outputs": [
            {"name": o.name, "type": o.type.WhichOneof("Type")} for o in spec.description.output
        ],
    }
    names_in = [i.name for i in spec.description.input]
    names_out = [o.name for o in spec.description.output]
    receipt["contract_matches_declared"] = (
        names_in == IN_NAMES and names_out == OUT_NAMES
    )
    receipt["has_mlstate"] = len(getattr(spec.description, "state", [])) > 0

    digest, size = dir_digest(pkg)
    receipt["artifact"] = {
        "path": "AudioEncoderCacheExplicit.mlpackage",
        "bytes": size,
        "sha256_recursive": digest,
        "spec_version": spec.specificationVersion,
    }
    receipt["status"] = "ok" if receipt["contract_matches_declared"] else "contract_mismatch"
    if receipt["status"] != "ok":
        receipt["stop_reason"] = (
            f"Core ML I/O names differ from contract: in={names_in} out={names_out}"
        )

    write(args.out, receipt)
    print(f"Stage D status: {receipt['status']}")
    print(f"  mlpackage bytes: {size:,}")
    print(f"  inputs : {names_in}")
    print(f"  outputs: {names_out}")
    print(f"  mlstate present: {receipt['has_mlstate']}")
    return 0


def write(out_dir, receipt):
    p = os.path.join(out_dir, "stage-d-receipt.json")
    blob = json.dumps(receipt, indent=2, sort_keys=True, default=str)
    with open(p, "w", encoding="utf-8") as fh:
        fh.write(blob)
        fh.flush()
        os.fsync(fh.fileno())
    print(f"wrote {p} ({len(blob)} bytes)")


if __name__ == "__main__":
    raise SystemExit(main())
