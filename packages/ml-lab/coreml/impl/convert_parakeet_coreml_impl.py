#!/usr/bin/env python3
"""
Parakeet -> CoreML/ANE converter (TDT v3 AND RNN-T).

RUNS ON: a CUDA Linux or WSL2 machine (NOT the Mac, NOT native Windows).
  env: python3.11 + torch + nemo_toolkit[asr] + coremltools>=8 + huggingface_hub
PRODUCES: 4 static-shape, FP16+palettized `.mlpackage` stages per model.
DOES NOT validate (no CoreML runtime off-Mac) -- copy the .mlpackage dir to the
Mac and run `coreml_bench.py` there to compile on the ANE + measure WER/RTF.

This follows FluidAudio's open (MIT/Apache) Parakeet CoreML recipe (static shapes +
6-bit palettization + 4-stage split + stateful decoder). Source weights are the open
NVIDIA checkpoints; we use no third-party proprietary models. Reference FluidAudio /
macparakeet (both open) if the NeMo-internal `# VERIFY` points below need tightening.

Lever knobs for the optimization sweep:
  --window-sec     lever #1 (static window: 3/5/8/15 ...)
  --palette-bits   lever #2 (4/6/8, or 0 = fp16-only)
  --deploy-target  lever #7 (macOS14/macOS15)
Levers #3-6 (decode round-trips, compute-units, ANE residency, I/O) are exercised
on the Mac side in coreml_bench.py.

Usage:
  python convert_parakeet_coreml.py --model tdt  --window-sec 15 --palette-bits 6 --out ./out/tdt_w15_p6
  python convert_parakeet_coreml.py --model rnnt --window-sec 15 --palette-bits 6 --out ./out/rnnt_w15_p6
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn

if os.environ.get("RIGHTKIT_ML_COREMLTOOLS_SITE"):
    sys.path.append(os.environ["RIGHTKIT_ML_COREMLTOOLS_SITE"])

# Both TDT and RNN-T load through EncDecRNNTBPEModel (TDT is an RNN-T decoding
# variant with an extra duration head). Unified ships as a .nemo checkpoint, so
# it uses the same wrappers after ASRModel.restore_from.
MODELS = {
    "tdt":     {"hf": "nvidia/parakeet-tdt-0.6b-v3",      "n_mel": 128, "tdt": True},
    "rnnt":    {"hf": "nvidia/parakeet-rnnt-0.6b",        "n_mel": 80,  "tdt": False},
    "unified": {
        "hf": "nvidia/parakeet-unified-en-0.6b",
        "nemo_filename": "parakeet-unified-en-0.6b.nemo",
        "n_mel": 128,
        "tdt": False,
        "blank_id": 1024,
        "pred_layers": 2,
        "pred_hidden": 640,
    },
}

# CoreML tensor contract (from FluidAudio's open Parakeet conversion):
#   AudioEncoder  out: encoder_output_embeds [1,1024,1,188] + joint_projected_encoder_output_embeds [1,640,1,188]
#   TextDecoder   out: decoder_output_projected [1,640] + new_state_1/2 [2,640]
#   MultimodalLogits out: logits [1,1030]  (= 1024 BPE + 1 blank + 5 TDT durations)
# TWO optimizations to match:
#   (1) FUSE the joint's encoder/decoder projections into EncoderWrap/DecoderWrap so the
#       per-step JointWrap is just add-640 -> act -> linear -> 1030 (shrinks the hot loop).
#   (2) Emit the ANE-preferred rank-4 layout [1, C, 1, T] from encoder/decoder.
# The wrappers below are the un-fused baseline; apply (1)+(2) once the baseline transcribes.

# NeMo mel hop is 10 ms (160 samples @ 16 kHz). Encoder subsampling factor 8
# (config.json) -> encoder time = mel_frames / 8.
SAMPLE_RATE = 16_000
HOP_S = 0.01
SUBSAMPLING = 8


def _self_test():
    unified = MODELS["unified"]
    assert unified["hf"] == "nvidia/parakeet-unified-en-0.6b"
    assert unified["n_mel"] == 128
    assert unified["tdt"] is False
    assert unified["blank_id"] == 1024
    assert unified["pred_layers"] == 2
    assert unified["pred_hidden"] == 640


def load_nemo(spec: dict):
    import nemo  # noqa
    hf_id = spec["hf"]
    print(f"[load] {hf_id}  (nemo {nemo.__version__}, torch {torch.__version__})")
    if spec.get("nemo_filename"):
        import tarfile
        import tempfile

        from huggingface_hub import hf_hub_download
        import nemo.collections.asr as nemo_asr
        from omegaconf import OmegaConf, open_dict

        nemo_path = hf_hub_download(repo_id=hf_id, filename=spec["nemo_filename"])
        with tarfile.open(nemo_path, "r:*") as archive:
            cfg_name = next(name for name in archive.getnames() if name.endswith("model_config.yaml"))
            cfg_file = archive.extractfile(cfg_name)
            if cfg_file is None:
                raise RuntimeError(f"{nemo_path} does not contain {cfg_name}")
            cfg = OmegaConf.create(cfg_file.read().decode("utf-8"))

        # Unified's checkpoint config is cache-aware. The current WSL NeMo env can
        # export the static CoreML graph, but its ConformerEncoder predates these
        # streaming-only knobs. Keep the weights/config otherwise intact.
        removed_encoder_keys = []
        changed_encoder_keys = {}
        with open_dict(cfg):
            encoder_cfg = cfg.get("encoder")
            if encoder_cfg is not None:
                for key in ("att_chunk_context_size", "conv_context_style"):
                    if key in encoder_cfg:
                        encoder_cfg.pop(key)
                        removed_encoder_keys.append(key)
                if encoder_cfg.get("att_context_style") == "chunked_limited_with_rc":
                    encoder_cfg.att_context_style = "regular"
                    changed_encoder_keys["att_context_style"] = "regular"
            if cfg.get("validation_ds") is None:
                cfg.validation_ds = OmegaConf.create({})

        with tempfile.TemporaryDirectory(prefix="hr_unified_coreml_") as tmp:
            override_path = Path(tmp) / "model_config.static_coreml.yaml"
            OmegaConf.save(cfg, override_path)
            if removed_encoder_keys:
                print(f"[load] static CoreML override removed encoder keys: {removed_encoder_keys}")
            if changed_encoder_keys:
                print(f"[load] static CoreML override changed encoder keys: {changed_encoder_keys}")
            model = nemo_asr.models.ASRModel.restore_from(
                nemo_path,
                map_location="cpu",
                override_config_path=str(override_path),
            )
        with open_dict(model.cfg):
            if model.cfg.get("validation_ds") is None:
                model.cfg.validation_ds = OmegaConf.create({})
    else:
        from nemo.collections.asr.models import EncDecRNNTBPEModel
        model = EncDecRNNTBPEModel.from_pretrained(model_name=hf_id, map_location="cpu")
    model.eval()
    return model


def write_vocab(model, out_dir: Path) -> None:
    tok = getattr(getattr(model, "tokenizer", None), "tokenizer", None)
    if tok is None or not hasattr(tok, "get_piece_size") or not hasattr(tok, "id_to_piece"):
        print("[vocab] WARNING: tokenizer pieces unavailable; copy vocab.txt before Mac validation")
        return
    pieces = [tok.id_to_piece(i) for i in range(tok.get_piece_size())]
    (out_dir / "vocab.txt").write_text("\n".join(pieces) + "\n", encoding="utf-8")
    print(f"[vocab] wrote {out_dir / 'vocab.txt'} ({len(pieces)} pieces)")


# ---------------------------------------------------------------------------
# Thin wrappers give each stage a CLEAN, static forward signature so torch.jit
# traces predictably and coremltools sees fixed shapes. The bodies call NeMo's
# real submodules -- those calls are the `# VERIFY` points (signatures vary a bit
# by NeMo version; tighten against the actual model).
# ---------------------------------------------------------------------------

from convert_parakeet_coreml_wrappers import DecoderWrap, EncoderWrap, JointWrap, MelWrap

def convert_stage(name, wrapper, example_inputs, input_specs, out_dir,
                  palette_bits, deploy_target, emit_onnx=False,
                  quant_mode="palette", linear_block_size=64, keep_conv_fp16=False,
                  palette_mode="kmeans", palette_group_size=0, palette_per_channel_scale=False,
                  compute_precision="fp16", linear_bits=4):
    """torch.jit.trace -> coremltools mlprogram -> (palettize | linear-quant) -> save.

    quant_mode:
      "palette" — kmeans LUT palettization at `palette_bits`. LUT-indexed weights are ANE-NATIVE
                  (the Neural Engine has hardware for them) so they compile fast and run on-ANE.
                  At nbits=4 this matches Windows' 4-bit precision TIER while staying ANE-friendly —
                  the right Mac path (affine-block is ANE-hostile: the compiler dequantizes it to
                  fp16, killing the size/speed win).
      "linear"  — 4-bit linear/affine block-wise quant to MIRROR the Windows ONNX encoder
                  (MatMulNBits: int4, block_size 64). Exact Windows scheme, but ANE-HOSTILE — use
                  only for DML/CPU parity studies, not the Mac ship path.
    keep_conv_fp16: skip Conv weights (keep fp16) to mirror Windows' tiering (4-bit matmuls + fp16
                  convs). Auto-on for linear; opt-in (--keep-conv-fp16) for palette.
    """
    import coremltools as ct

    wrapper.eval()
    with torch.no_grad():
        if hasattr(wrapper, "prepare_static_trace"):
            wrapper.prepare_static_trace(*example_inputs)
        source_output = wrapper(*example_inputs)
        traced = torch.jit.trace(wrapper, example_inputs, check_trace=False)
        traced_output = traced(*example_inputs)

    def check_trace(source, replay):
        if isinstance(source, torch.Tensor):
            if source.shape != replay.shape or not torch.isfinite(source).all() or not torch.isfinite(replay).all():
                raise RuntimeError(f"{name}: nonfinite or mismatched source/trace output")
            max_diff = (source - replay).abs().max().item()
            if max_diff > 1e-4:
                raise RuntimeError(f"{name}: source/trace max difference {max_diff}")
            print(f"[trace] {name} shape={tuple(source.shape)} finite=true max_diff={max_diff:.3g}")
        else:
            for source_part, replay_part in zip(source, replay, strict=True):
                check_trace(source_part, replay_part)

    check_trace(source_output, traced_output)

    target = {"macOS14": ct.target.macOS14, "macOS15": ct.target.macOS15}[deploy_target]
    precision = {"fp16": ct.precision.FLOAT16, "fp32": ct.precision.FLOAT32}[compute_precision]
    mlmodel = ct.convert(
        traced,
        inputs=input_specs,
        convert_to="mlprogram",
        compute_precision=precision,
        minimum_deployment_target=target,
        compute_units=ct.ComputeUnit.CPU_AND_NE,  # request ANE residency (lever #4 default)
    )

    if quant_mode == "linear":
        # 4-bit linear/affine block-wise quant — mirror of the Windows MatMulNBits encoder
        # (bits=4, block_size=64). per_block granularity matches the ONNX block scheme.
        # IMPORTANT parity detail: the Windows onnxruntime MatMulNBits quantizer ONLY touches
        # GEMM/linear weights and leaves Conv layers (the quant-sensitive FastConformer depthwise
        # convs) in fp16. We must do the same — a global config over-quantizes the convs, shrinks
        # the file ~120 MB, and risks accuracy. Skip conv to truly match Windows.
        from coremltools.optimize.coreml import (
            OpLinearQuantizerConfig, OptimizationConfig, linear_quantize_weights,
        )
        lin = OpLinearQuantizerConfig(
            mode="linear_symmetric", dtype=f"int{linear_bits}",
            granularity="per_block", block_size=linear_block_size,
        )
        cfg = OptimizationConfig(
            global_config=lin,
            op_type_configs={"conv": None},   # keep Conv fp16, like Windows MatMulNBits
        )
        mlmodel = linear_quantize_weights(mlmodel, cfg)
    elif palette_bits and palette_bits > 0:
        # Lever #2: weight palettization (LUT) — ANE-native. SW uses 6-bit; 4-bit matches the
        # Windows precision tier while staying ANE-friendly.
        # "match-best" recipe to approximate the Windows 4-bit affine-block-64 encoder while
        # staying LUT/ANE-native: mode=uniform (uniform levels == affine-linear spacing, vs kmeans
        # clusters), per-grouped-channel granularity with group_size 64 (== Windows block_size 64),
        # and per-channel scale (mimics affine's per-block fp16 scales). convs stay fp16.
        from coremltools.optimize.coreml import (
            OpPalettizerConfig, OptimizationConfig, palettize_weights,
        )
        pal_kw = dict(mode=palette_mode, nbits=palette_bits,
                      enable_per_channel_scale=palette_per_channel_scale)
        if palette_group_size > 0:
            from coremltools.optimize.coreml._config import OpPalettizerConfig as _P
            G = type(OpPalettizerConfig(mode="uniform", nbits=palette_bits).granularity)
            pal_kw.update(granularity=G.PER_GROUPED_CHANNEL, group_size=palette_group_size)
        cfg = OptimizationConfig(
            global_config=OpPalettizerConfig(**pal_kw),
            op_type_configs=({"conv": None} if keep_conv_fp16 else {}),  # mirror Windows: convs fp16
        )
        mlmodel = palettize_weights(mlmodel, cfg)

    out = Path(out_dir) / f"{name}.mlpackage"
    mlmodel.save(str(out))
    print(f"[saved] {out}  ({sum(f.stat().st_size for f in out.rglob('*'))/1e6:.1f} MB)")
    if emit_onnx:
        onnx_path = Path(out_dir) / f"{name}.onnx"
        try:
            input_names = [sp.name for sp in input_specs]
            torch.onnx.export(wrapper, example_inputs, str(onnx_path),
                              input_names=input_names, opset_version=17,
                              do_constant_folding=True, dynamic_axes=None)
            print(f"[onnx]  {onnx_path}  ({onnx_path.stat().st_size/1e6:.1f} MB)")
        except Exception as e:
            print(f"[onnx]  FAILED {name}: {repr(e)[:200]}")
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", choices=list(MODELS))
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--window-sec", type=float, default=15.0, help="static encoder window (lever #1)")
    ap.add_argument("--palette-bits", type=int, default=6, choices=[0, 4, 6, 8], help="lever #2 (0=fp16 only)")
    ap.add_argument("--quant-mode", choices=["palette", "linear"], default="palette",
                    help="palette=kmeans LUT (default); linear=4-bit affine block-wise to mirror the Windows MatMulNBits encoder")
    ap.add_argument("--linear-block-size", type=int, default=64, help="block_size for --quant-mode linear (Windows encoder = 64)")
    ap.add_argument("--keep-conv-fp16", action="store_true", help="keep Conv weights fp16 (mirror Windows tiering: 4-bit matmuls + fp16 convs); applies to --quant-mode palette")
    ap.add_argument("--palette-mode", choices=["kmeans", "uniform"], default="kmeans",
                    help="palette level spacing: kmeans (non-uniform, default/legacy 6-bit) | uniform (== affine-linear levels, the Windows-match-best path)")
    ap.add_argument("--palette-group-size", type=int, default=0,
                    help="0=per-tensor (legacy, ANE-OK); >0 = per-grouped-channel group_size. "
                         "WARNING: block/group scaling (e.g. 64) FAILS ANECCompile on the Apple ANE "
                         "(verified 2026-06-15) — same failure as --quant-mode linear. ANE needs a "
                         "SIMPLE per-tensor palette (group_size 0, no per-channel scale).")
    ap.add_argument("--palette-per-channel-scale", action="store_true",
                    help="add per-channel fp16 scale to the LUT. WARNING: ANE-INCOMPATIBLE (ANECCompile "
                         "fails) — only for off-ANE/study builds. Leave off for any ANE ship encoder.")
    ap.add_argument("--encoder-only", action="store_true", help="convert ONLY AudioEncoder (decoder/joint already matched via int8perchan dropin)")
    ap.add_argument("--mel-encoder-only", action="store_true", help="convert MelSpectrogram and AudioEncoder only; reuse shipped TextDecoder and Joint")
    ap.add_argument("--deploy-target", choices=["macOS14", "macOS15"], default="macOS14", help="lever #7")
    ap.add_argument("--skip-mel", action="store_true", help="do mel host-side instead of in CoreML")
    ap.add_argument("--emit-onnx", action="store_true", help="also export fp32 static-shape ONNX next to each .mlpackage (Windows W1/W2 artifacts; unpalettized)")
    ap.add_argument("--out")
    args = ap.parse_args()

    if args.self_test:
        _self_test()
        print("self-test ok")
        return 0
    if not args.model:
        raise SystemExit("--model is required unless --self-test is used")
    if not args.out:
        raise SystemExit("--out is required unless --self-test is used")
    if args.mel_encoder_only and (args.encoder_only or args.skip_mel):
        raise SystemExit("--mel-encoder-only requires MelSpectrogram and AudioEncoder")

    spec = MODELS[args.model]
    n_mel = spec["n_mel"]
    mel_frames = int(round(args.window_sec / HOP_S))          # static encoder time-in
    enc_frames = math.ceil(mel_frames / SUBSAMPLING)          # encoder time-out
    samples = int(round(args.window_sec * SAMPLE_RATE))
    out_dir = Path(args.out); out_dir.mkdir(parents=True, exist_ok=True)

    print(f"[cfg] model={args.model} hf={spec['hf']} n_mel={n_mel} "
          f"window={args.window_sec}s mel_frames={mel_frames} enc_frames={enc_frames} "
          f"palette={args.palette_bits}bit target={args.deploy_target}")

    import coremltools as ct
    model = load_nemo(spec)
    write_vocab(model, out_dir)

    # Discover decoder dims for the stateful state tensors. VERIFY against the model.
    pred_cfg = model.cfg.decoder.prednet
    num_layers = int(pred_cfg.get("pred_rnn_layers", spec.get("pred_layers", 1)))
    hidden = int(pred_cfg.get("pred_hidden", spec.get("pred_hidden")))
    enc_dim = int(model.cfg.encoder.d_model)
    print(f"[dims] enc_d_model={enc_dim} pred_layers={num_layers} pred_hidden={hidden}")

    # 1) Mel (optional)
    if not args.skip_mel and not args.encoder_only:
        audio = torch.randn(1, samples)
        length = torch.tensor([samples], dtype=torch.int64)
        convert_stage(
            "MelSpectrogram", MelWrap(model), (audio, length),
            [ct.TensorType(name="audio", shape=(1, samples), dtype=np.float16),
             ct.TensorType(name="audio_len", shape=(1,), dtype=np.int32)],
            out_dir, args.palette_bits, args.deploy_target, args.emit_onnx,
        )

    # 2) Encoder (the heavy one; static window is the whole point)
    mel = torch.randn(1, n_mel, mel_frames)
    mlen = torch.tensor([mel_frames], dtype=torch.int64)
    convert_stage(
        "AudioEncoder", EncoderWrap(model), (mel, mlen),
        [ct.TensorType(name="melspectrogram_features", shape=(1, n_mel, mel_frames), dtype=np.float16),
         ct.TensorType(name="mel_len", shape=(1,), dtype=np.int32)],
        out_dir, args.palette_bits, args.deploy_target, args.emit_onnx,
        quant_mode=args.quant_mode, linear_block_size=args.linear_block_size,
        keep_conv_fp16=args.keep_conv_fp16,
        palette_mode=args.palette_mode, palette_group_size=args.palette_group_size,
        palette_per_channel_scale=args.palette_per_channel_scale,
    )

    # 3) Decoder (stateful prediction net, single step)
    if not args.encoder_only and not args.mel_encoder_only:
        token = torch.zeros(1, 1, dtype=torch.int32)
        h = torch.zeros(num_layers, 1, hidden)
        c = torch.zeros(num_layers, 1, hidden)
        convert_stage(
            "TextDecoder", DecoderWrap(model), (token, h, c),
            [ct.TensorType(name="decoder_input_ids", shape=(1, 1), dtype=np.int32),
             ct.TensorType(name="state_h", shape=(num_layers, 1, hidden), dtype=np.float16),
             ct.TensorType(name="state_c", shape=(num_layers, 1, hidden), dtype=np.float16)],
            out_dir, args.palette_bits, args.deploy_target, args.emit_onnx,
            quant_mode=args.quant_mode, linear_block_size=args.linear_block_size,
        )

    # 4) Joint (single enc step x single dec step -> logits)
    if not args.encoder_only and not args.mel_encoder_only:
        enc_step = torch.randn(1, enc_dim)
        dec_step = torch.randn(1, hidden)
        convert_stage(
            "Joint", JointWrap(model, spec["tdt"]), (enc_step, dec_step),
            [ct.TensorType(name="encoder_step", shape=(1, enc_dim), dtype=np.float16),
             ct.TensorType(name="decoder_step", shape=(1, hidden), dtype=np.float16)],
            out_dir, args.palette_bits, args.deploy_target, args.emit_onnx,
            quant_mode=args.quant_mode, linear_block_size=args.linear_block_size,
        )

    # Sidecar so the Mac bench knows how to drive the stages.
    meta = {
        "model": args.model, "hf": spec["hf"], "is_tdt": spec["tdt"],
        "n_mel": n_mel, "window_sec": args.window_sec, "mel_frames": mel_frames,
        "enc_frames": enc_frames, "enc_d_model": enc_dim,
        "pred_layers": num_layers, "pred_hidden": hidden,
        "palette_bits": args.palette_bits, "deploy_target": args.deploy_target,
        "skip_mel": args.skip_mel,
        "exported_stages": (["AudioEncoder"] if args.encoder_only else
                            ["MelSpectrogram", "AudioEncoder"] if args.mel_encoder_only else
                            ["AudioEncoder", "TextDecoder", "Joint"] if args.skip_mel else
                            ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"]),
        "blank_id": int(getattr(model.decoder, "blank_idx", spec.get("blank_id", 0))),
        # TDT durations live in cfg.model_defaults / cfg.loss.durations -- VERIFY.
        "tdt_durations": list(model.cfg.get("model_defaults", {}).get("tdt_durations", []))
                          if spec["tdt"] else None,
    }
    (out_dir / "pipeline.json").write_text(json.dumps(meta, indent=2))
    print(f"[done] wrote {out_dir}/pipeline.json -- copy {out_dir} to the Mac and run coreml_bench.py")
    return 0


if __name__ == "__main__":
    sys.exit(main())
