#!/usr/bin/env python3
"""Convert a Silero VAD 16 kHz ONNX graph (opset 15) to CoreML (fp32, fixed shapes).

Usage (environment: ../../requirements/coreml.txt):
    python3 convert_silero_vad_coreml.py <silero_vad_16k_op15.onnx> [<out.mlpackage>]
    xcrun coremlcompiler compile silero_vad_16k.mlpackage .

silero_torch.py is a numerically exact PyTorch twin of that graph at the fixed [1,576]/[2,1,128]
shapes (max |dProb| about 1e-7 against onnxruntime). Check parity against the ONNX model on your
own clips before shipping the compiled .mlmodelc.
"""

from pathlib import Path

import numpy as np
import torch
import coremltools as ct

from silero_torch import SileroVad16k, load_weights



def main():
    import sys

    if len(sys.argv) not in (2, 3):
        raise SystemExit("usage: convert_silero_vad_coreml.py <silero_vad_16k_op15.onnx> [<out.mlpackage>]")
    onnx_path = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) == 3 else Path.cwd() / "silero_vad_16k.mlpackage"
    net = SileroVad16k(load_weights(onnx_path)).eval()
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
    ml.save(str(out))
    print(f"saved {out}")


if __name__ == "__main__":
    main()
