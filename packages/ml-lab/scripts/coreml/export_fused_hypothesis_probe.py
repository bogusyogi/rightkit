#!/usr/bin/env python3
"""Export a source-weight Parakeet predictor+joint probe graph.

This is intentionally separate from the shipping four-stage assets.  It never
downloads a checkpoint: pass a local ``.nemo`` source file (or run ``--preflight``).
Each exported package supports flexible batch B=1..8; ``--batch-sizes`` selects
sanity cases. Raw Joint logits use production host FP32 normalization; this
avoids redundant graph normalization & its FP16 rounding. Use a fresh output
path to avoid stale compiled CoreML caches.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
IMPL = Path(os.environ.get("RIGHTKIT_ML_COREML_IMPL", Path(__file__).resolve().parent / "impl"))
DEFAULT_OUT = ROOT / ".audit/2026-10-02-macos-option-qualification/export"


def sha256_path(path: Path) -> str:
    digest = hashlib.sha256()
    if path.is_file():
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
        return digest.hexdigest()
    for child in sorted(path.rglob("*")):
        if child.is_file():
            digest.update(str(child.relative_to(path)).replace("\\", "/").encode())
            digest.update(b"\0")
            with child.open("rb") as handle:
                for block in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(block)
    return digest.hexdigest()


def package_available(name: str) -> bool:
    return importlib.util.find_spec(name) is not None


def cached_package_paths(name: str) -> list[str]:
    """Report uv cache copies without putting them on sys.path."""
    archive = Path.home() / ".cache/uv/archive-v0"
    found: list[str] = []
    if archive.exists():
        for site in archive.glob("*/lib/python*/site-packages"):
            if (site / name).exists() or any(site.glob(f"{name}-*.dist-info")):
                found.append(str(site))
    return found


def locate_source(explicit: Path | None) -> Path | None:
    if explicit is not None:
        return explicit.expanduser().resolve() if explicit.exists() else None
    roots = [
        Path(value).expanduser()
        for value in (os.environ.get("RIGHTKIT_ML_PARakeet_SOURCE"), os.environ.get("HF_HOME"))
        if value
    ]
    roots += [Path.home() / ".cache/huggingface/hub", Path("/Volumes/D/claude/tools/.cache")]
    names = ("*.nemo", "*.safetensors", "pytorch_model*.bin", "*.pth", "*.pt")
    for root in roots:
        if not root.exists():
            continue
        for pattern in names:
            for candidate in root.glob(f"**/{pattern}"):
                if "parakeet" in str(candidate).lower() or "tdt" in str(candidate).lower():
                    return candidate
    return None


def build_fused_from_state(torch, state, joint_rank4=False, joint_raw=True):
    """Rebuild only decoder+joint from NeMo checkpoint tensors.

    Keys/shapes are the native NeMo RNNTDecoder/RNNTJoint state layout. This
    avoids importing NeMo on hosts where its optional runtime is unavailable.
    """
    import torch.nn as nn

    class _FusedFromState(nn.Module):
        def __init__(self):
            super().__init__()
            emb = state["decoder.prediction.embed.weight"]
            self.embed = nn.Embedding(emb.shape[0], emb.shape[1])
            self.embed.weight.data.copy_(emb)
            self.rnn0 = nn.LSTM(emb.shape[1], emb.shape[1], batch_first=True)
            self.rnn1 = nn.LSTM(emb.shape[1], emb.shape[1], batch_first=True)
            for layer, rnn in ((0, self.rnn0), (1, self.rnn1)):
                prefix = f"decoder.prediction.dec_rnn.lstm."
                rnn.weight_ih_l0.data.copy_(state[prefix + f"weight_ih_l{layer}"])
                rnn.weight_hh_l0.data.copy_(state[prefix + f"weight_hh_l{layer}"])
                rnn.bias_ih_l0.data.copy_(state[prefix + f"bias_ih_l{layer}"])
                rnn.bias_hh_l0.data.copy_(state[prefix + f"bias_hh_l{layer}"])
            self.enc = nn.Linear(1024, 640)
            self.pred = nn.Linear(640, 640)
            self.out = nn.Linear(640, state["joint.joint_net.2.weight"].shape[0])
            self.enc.weight.data.copy_(state["joint.enc.weight"])
            self.enc.bias.data.copy_(state["joint.enc.bias"])
            self.pred.weight.data.copy_(state["joint.pred.weight"])
            self.pred.bias.data.copy_(state["joint.pred.bias"])
            self.out.weight.data.copy_(state["joint.joint_net.2.weight"])
            self.out.bias.data.copy_(state["joint.joint_net.2.bias"])

        def forward(self, token, state_h, state_c, encoder_step):
            x = self.embed(token.long())
            x, (h0, c0) = self.rnn0(x, (state_h[0:1], state_c[0:1]))
            x, (h1, c1) = self.rnn1(x, (state_h[1:2], state_c[1:2]))
            g = x
            enc_proj = self.enc(encoder_step)
            pred_proj = self.pred(g)
            if joint_rank4:
                # Match installed Joint's [B,1,1,D] linear/softmax layout.
                enc_proj = enc_proj.unsqueeze(1).unsqueeze(1)
                pred_proj = pred_proj.unsqueeze(1)
            else:
                enc_proj = enc_proj.unsqueeze(1)
            fused = torch.relu(enc_proj + pred_proj)
            # NeMo RNNTJoint.joint (and installed Joint.mlmodelc) returns
            # log-softmax scores, despite host code calling them logits.
            raw = self.out(fused)
            if joint_raw:
                logits = raw
            else:
                logits = torch.log_softmax(raw, dim=-1)
            if not joint_rank4:
                logits = logits.unsqueeze(1)
            return logits, g, torch.cat((h0, h1), dim=0), torch.cat((c0, c1), dim=0)

    return _FusedFromState


def load_local_model(source: Path):
    # The existing converter's load_nemo uses from_pretrained and may download.
    # This probe accepts only an explicitly local checkpoint.
    from nemo.collections.asr.models import EncDecRNNTBPEModel

    return EncDecRNNTBPEModel.restore_from(str(source), map_location="cpu")


def preflight(source_arg: Path | None, state_arg: Path | None, out: Path) -> dict:
    source = locate_source(source_arg)
    state = state_arg.expanduser().resolve() if state_arg and state_arg.exists() else None
    result = {
        "python": sys.executable,
        "python_version": platform.python_version(),
        "platform": platform.platform(),
        "source": str(source) if source else None,
        "source_sha256": sha256_path(source) if source else None,
        "state_dict": str(state) if state else None,
        "state_dict_sha256": sha256_path(state) if state else None,
        "packages": {name: package_available(name) for name in ("torch", "coremltools", "nemo")},
        "cached_package_roots": {
            name: cached_package_paths(name) for name in ("torch", "coremltools", "nemo")
        },
        "compiled_assets_are_source": False,
        "installed_asset_root": str(
            Path.home() / "Library/Application Support/HeardRightNext/models/parakeet-tdt-v3"
        ),
    }
    out.mkdir(parents=True, exist_ok=True)
    (out / "preflight.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def export(args: argparse.Namespace) -> int:
    for existing in (args.out / "fused_predictor_joint.mlpackage",
                     args.out / "fused_predictor_joint.mlmodelc"):
        if existing.exists():
            raise RuntimeError(f"refusing to overwrite existing export: {existing}")
    report = preflight(args.source, args.state_dict, args.out)
    required = ("torch", "coremltools") if args.state_dict else ("torch", "coremltools", "nemo")
    missing = [name for name in required if not report["packages"][name]]
    if missing:
        raise RuntimeError(
            "missing provisioned packages: " + ", ".join(missing) +
            "; install/download is intentionally disabled"
        )
    source = locate_source(args.source)
    state_path = args.state_dict.expanduser().resolve() if args.state_dict else None
    if state_path is None and source is None:
        raise RuntimeError("no local Parakeet .nemo/source checkpoint found")

    sys.path.insert(0, str(IMPL))
    import numpy as np
    import torch
    import coremltools as ct

    if state_path is not None:
        state = torch.load(state_path, map_location="cpu", weights_only=True)
        layers, hidden, enc_dim = 2, 640, 1024
        vocab = int(state["joint.joint_net.2.weight"].shape[0])
        fused = build_fused_from_state(torch, state, args.joint_rank4, args.joint_raw)().eval()
    else:
        model = load_local_model(source)
        model.eval()
        pred_cfg = model.cfg.decoder.prednet
        layers = int(pred_cfg.get("pred_rnn_layers", 2))
        hidden = int(pred_cfg.get("pred_hidden", 640))
        enc_dim = int(model.cfg.encoder.d_model)
        state = model.state_dict()
        vocab = int(state["joint.joint_net.2.weight"].shape[0])
        fused = build_fused_from_state(torch, state, args.joint_rank4, args.joint_raw)().eval()
    torch.manual_seed(20261002)

    graph_report = {
        "source_sha256": sha256_path(source) if source else None,
        "state_dict_sha256": sha256_path(state_path) if state_path else None,
        "dims": {"pred_layers": layers, "pred_hidden": hidden, "enc_d_model": enc_dim, "vocab": vocab},
        "outputs": ["logits", "predictor_g", "state_h_out", "state_c_out"],
        # Installed TextDecoder MIL is fp16; installed Joint MIL uses affine
        # int8, symmetric per output channel (MIL affine op, zero_point=0).
        "quantization": {"predictor": "fp16", "joint": "int8_symmetric_per_channel"},
        "joint_layout": "rank4" if args.joint_rank4 else "rank3",
        "joint_norm_precision": "host_fp32" if args.joint_raw else "graph_fp16",
        "joint_output": "raw_linear" if args.joint_raw else "log_softmax",
        "joint_precision": "fp16",
        "compute_unit_controls": ["CPU_ONLY", "CPU_AND_NE"],
        "batches": [],
    }
    for batch in args.batch_sizes:
        token_count = int(state["decoder.prediction.embed.weight"].shape[0])
        token = torch.randint(0, token_count, (batch, 1), dtype=torch.int32)
        h = torch.randn(layers, batch, hidden)
        c = torch.randn(layers, batch, hidden)
        enc = torch.randn(batch, enc_dim)
        with torch.no_grad():
            outputs = fused(token, h, c, enc)
            scalar_rows = [fused(token[i:i + 1], h[:, i:i + 1], c[:, i:i + 1], enc[i:i + 1])
                           for i in range(batch)]
        maxima = []
        for output_index, (output, rows) in enumerate(zip(outputs, zip(*scalar_rows))):
            # logits/g batch on dim 0; recurrent states batch on dim 1.
            maxima.append(max(float(((output[:, i:i + 1] if output_index >= 2 else output[i:i + 1]) - row).abs().max())
                              for i, row in enumerate(rows)))
        graph_report["batches"].append({"batch": batch, "torch_vs_scalar_max_abs": maxima,
                                        "shapes": [list(value.shape) for value in outputs]})

    # One graph with a shared flexible B keeps OneStage loading simple. CoreML
    # before iOS18 permits only one EnumeratedShapes input, while RangeDim can
    # be shared across all four tensors and covers the same B=1..8 probes.
    batches = sorted(set(args.batch_sizes))
    batch_dim = ct.RangeDim(lower_bound=min(batches), upper_bound=max(batches), default=1)
    shape_sets = {
        "token": (batch_dim, 1),
        "state": (layers, batch_dim, hidden),
        "encoder": (batch_dim, enc_dim),
    }
    specs = [
        ct.TensorType(name="decoder_input_ids", shape=shape_sets["token"], dtype=np.int32),
        ct.TensorType(name="state_h", shape=shape_sets["state"], dtype=np.float16),
        ct.TensorType(name="state_c", shape=shape_sets["state"], dtype=np.float16),
        ct.TensorType(name="encoder_step", shape=shape_sets["encoder"], dtype=np.float16),
    ]
    # Decoder embedding owns token range; Joint output class count can exceed it.
    token_count = int(state["decoder.prediction.embed.weight"].shape[0])
    token = torch.randint(0, token_count, (1, 1), dtype=torch.int32)
    h = torch.randn(layers, 1, hidden)
    c = torch.randn(layers, 1, hidden)
    enc = torch.randn(1, enc_dim)
    traced = torch.jit.trace(fused, (token, h, c, enc), check_trace=False)
    out_dir = args.out / "fused_predictor_joint.mlpackage"
    compute_precision = ct.precision.FLOAT16
    converted = ct.convert(
        traced, inputs=specs, convert_to="mlprogram",
        # Native Stage::read_f16 reads every output; host scoring uses f32.
        outputs=[ct.TensorType(name=name, dtype=np.float16) for name in graph_report["outputs"]],
        compute_precision=compute_precision,
        minimum_deployment_target=ct.target.macOS14,
        compute_units=ct.ComputeUnit.CPU_AND_NE,
    )
    # The fused graph has LSTM predictor ops and linear Joint ops. A global
    # config would quantize both; op-type selection keeps predictor state math
    # fp16 while matching installed Joint's affine per-channel weights.
    from coremltools.optimize.coreml import (
        OpLinearQuantizerConfig, OptimizationConfig, linear_quantize_weights,
    )
    # Existing Joint.mlmodelc uses per-output-channel symmetric int8
    # (zero_point is exactly 0 for all three linear weights).
    joint_int8 = OpLinearQuantizerConfig(
        mode="linear_symmetric", dtype="int8", granularity="per_channel"
    )
    converted = linear_quantize_weights(
        converted,
        OptimizationConfig(op_type_configs={"linear": joint_int8, "lstm": None}),
    )
    for existing in (out_dir, out_dir.with_suffix(".mlmodelc")):
        if existing.exists():
            raise RuntimeError(f"refusing to overwrite existing export: {existing}")
    converted.save(str(out_dir))
    graph_report["export"] = str(out_dir)
    graph_report["graph_sha256"] = sha256_path(out_dir)
    graph_report["shape_mode"] = "range_dim"

    (args.out / "fused_probe.json").write_text(json.dumps(graph_report, indent=2) + "\n")
    print(json.dumps(graph_report, indent=2))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, help="local Parakeet .nemo checkpoint; never downloaded")
    parser.add_argument("--state-dict", type=Path, help="local decoder+joint torch state extracted from .nemo")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--batch-sizes", type=int, nargs="+", default=[1, 2, 4, 8])
    parser.add_argument("--preflight", action="store_true", help="write dependency/source report and stop")
    parser.add_argument("--joint-rank4", action="store_true", help="match split Joint's rank-4 batched layout")
    output_group = parser.add_mutually_exclusive_group()
    output_group.add_argument("--joint-raw", dest="joint_raw", action="store_true",
                              help="raw Joint logits; existing host FP32 normalization (default)")
    output_group.add_argument("--joint-log-softmax", dest="joint_raw", action="store_false",
                              help="historical graph FP16 normalization; diagnostic reproduction only")
    parser.set_defaults(joint_raw=True)
    args = parser.parse_args()
    if 1 not in args.batch_sizes or any(value < 1 or value > 8 for value in args.batch_sizes):
        parser.error("batch sizes must include1 & stay within1..8")
    if args.preflight:
        print(json.dumps(preflight(args.source, args.state_dict, args.out), indent=2))
        return 0
    return export(args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as exc:
        print(f"[blocked] {exc}", file=sys.stderr)
        raise SystemExit(2)
