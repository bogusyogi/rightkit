#!/usr/bin/env python3
"""Convert HR's shipped Silero VAD ONNX to CoreML (fp32, fixed shapes).

Usage (venv needs torch + coremltools + onnx):
    ~/.hr-coreml-venv/bin/python convert_silero_vad_coreml.py
    xcrun coremlcompiler compile silero_vad_16k.mlpackage .

Then run parity_check.py (Python-level) and the engine's
`cargo run --example vad_parity_check` (Rust-level) before staging the
.mlmodelc into src-tauri/resources/vad/.

The source of truth is src-tauri/resources/vad/silero_vad_16k_op15.onnx;
silero_torch.py is a numerically-exact PyTorch twin of that graph at the
fixed [1,576]/[2,1,128] shapes (verified max|dProb| ~1e-7 vs onnxruntime).
"""

from pathlib import Path

import numpy as np
import torch
import coremltools as ct

from silero_torch import SileroVad16k, load_weights

ONNX = (
    Path(__file__).resolve().parents[3]
    / "src-tauri/resources/vad/silero_vad_16k_op15.onnx"
)
OUT = Path(__file__).with_name("silero_vad_16k.mlpackage")


def main():
    net = SileroVad16k(load_weights(ONNX)).eval()
    traced = torch.jit.trace(net, (torch.zeros(1, 576), torch.zeros(2, 1, 128)))
    ml = ct.convert(
        traced,
        inputs=[
            ct.TensorType(name="input", shape=(1, 576), dtype=np.float32),
            ct.TensorType(name="state_in", shape=(2, 1, 128), dtype=np.float32),
        ],
        outputs=[
            ct.TensorType(name="output", dtype=np.float32),
            ct.TensorType(name="state_out", dtype=np.float32),
        ],
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT32,
        minimum_deployment_target=ct.target.macOS13,
    )
    ml.save(str(OUT))
    print(f"saved {OUT}")


if __name__ == "__main__":
    main()
