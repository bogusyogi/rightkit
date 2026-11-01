#!/usr/bin/env python3
"""Export a decoder-only Core ML precision matrix from NVIDIA Parakeet TDT v3."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def mac_variant_specs() -> dict[str, dict]:
    return {
        "fp32-full": {"compute_precision": "fp32", "quant_mode": "palette", "palette_bits": 0},
        "fp16-full": {"compute_precision": "fp16", "quant_mode": "palette", "palette_bits": 0},
        "palette8-kmeans": {"compute_precision": "fp16", "quant_mode": "palette", "palette_bits": 8, "palette_mode": "kmeans"},
        "palette8-uniform": {"compute_precision": "fp16", "quant_mode": "palette", "palette_bits": 8, "palette_mode": "uniform"},
        "linear8-symmetric": {"compute_precision": "fp16", "quant_mode": "linear", "palette_bits": 0, "linear_bits": 8, "minimum_target": "macOS15"},
        "palette6-kmeans": {"compute_precision": "fp16", "quant_mode": "palette", "palette_bits": 6, "palette_mode": "kmeans"},
    }


def repair_manifest(out: Path, deploy_target: str) -> dict:
    specs = mac_variant_specs()
    variants = {}
    for name, cfg in specs.items():
        variant_dir = out / name
        if all((variant_dir / package).is_dir() for package in ("TextDecoder.mlpackage", "Joint.mlpackage")):
            variants[name] = cfg
    manifest = {
        "model": "nvidia/parakeet-tdt-0.6b-v3",
        "deploy_target": deploy_target,
        "variants": variants,
    }
    out.mkdir(parents=True, exist_ok=True)
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--deploy-target", choices=["macOS14", "macOS15"], default="macOS14")
    parser.add_argument("--variants", nargs="*", choices=list(mac_variant_specs()))
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--repair-manifest", action="store_true")
    args = parser.parse_args()
    specs = mac_variant_specs()
    selected = args.variants or list(specs)
    if args.dry_run:
        print(json.dumps({name: specs[name] for name in selected}, indent=2))
        return 0
    if args.repair_manifest:
        manifest = repair_manifest(args.out, args.deploy_target)
        print(json.dumps(manifest, indent=2))
        return 0

    impl_dir = Path(__file__).with_name("impl")
    sys.path.insert(0, str(impl_dir))
    import numpy as np
    import torch
    import coremltools as ct
    from convert_parakeet_coreml_impl import MODELS, convert_stage, load_nemo, write_vocab
    from convert_parakeet_coreml_wrappers import DecoderWrap, JointWrap

    args.out.mkdir(parents=True, exist_ok=True)
    spec = MODELS["tdt"]
    model = load_nemo(spec)
    write_vocab(model, args.out)
    pred_cfg = model.cfg.decoder.prednet
    layers = int(pred_cfg.get("pred_rnn_layers", 2))
    hidden = int(pred_cfg.get("pred_hidden", 640))
    enc_dim = int(model.cfg.encoder.d_model)
    token = torch.zeros(1, 1, dtype=torch.int32)
    h = torch.zeros(layers, 1, hidden)
    c = torch.zeros(layers, 1, hidden)
    enc_step = torch.randn(1, enc_dim)
    dec_step = torch.randn(1, hidden)
    for name in selected:
        cfg = specs[name]
        out = args.out / name
        out.mkdir(parents=True, exist_ok=True)
        deploy_target = "macOS15" if "macOS15" in (args.deploy_target, cfg.get("minimum_target")) else "macOS14"
        state_dtype = np.float32 if cfg["compute_precision"] == "fp32" else np.float16
        common = dict(
            palette_bits=cfg["palette_bits"], deploy_target=deploy_target,
            quant_mode=cfg["quant_mode"], palette_mode=cfg.get("palette_mode", "kmeans"),
            compute_precision=cfg["compute_precision"], linear_bits=cfg.get("linear_bits", 4),
        )
        convert_stage(
            "TextDecoder", DecoderWrap(model), (token, h, c),
            [ct.TensorType(name="decoder_input_ids", shape=(1, 1), dtype=np.int32),
             ct.TensorType(name="state_h", shape=(layers, 1, hidden), dtype=state_dtype),
             ct.TensorType(name="state_c", shape=(layers, 1, hidden), dtype=state_dtype)],
            out, **common,
        )
        convert_stage(
            "Joint", JointWrap(model, True), (enc_step, dec_step),
            [ct.TensorType(name="encoder_step", shape=(1, enc_dim), dtype=state_dtype),
             ct.TensorType(name="decoder_step", shape=(1, hidden), dtype=state_dtype)],
            out, **common,
        )
        repair_manifest(args.out, args.deploy_target)
        print(f"[complete] {name}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
