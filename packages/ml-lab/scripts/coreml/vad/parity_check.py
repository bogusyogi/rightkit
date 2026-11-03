#!/usr/bin/env python3
"""Stateful parity: CoreML Silero VAD vs the shipped ONNX via onnxruntime.

Replays real 16 kHz mono wavs through both engines with vad.rs's exact
framing (512-sample frames, 64-sample context, clamp to +/-1), compares
per-frame probabilities and decisions across a threshold sweep.

2026-07-18 result (10 wavs, adaptive/win_raws): 12,317 frames,
max|dProb| = 1.15e-5, zero decision disagreements at 0.20/0.30/0.40/0.50.

    ~/.hr-coreml-venv/bin/python parity_check.py <wav> [<wav>...]
"""

import glob
import sys
import wave
from pathlib import Path

import numpy as np
import coremltools as ct
import onnxruntime as rt

HERE = Path(__file__).resolve().parent
ONNX = HERE.parents[2] / "src-tauri/resources/vad/silero_vad_16k_op15.onnx"
MLPKG = HERE / "silero_vad_16k.mlpackage"
THRESH = [0.20, 0.30, 0.40, 0.50]


def frames(pcm):
    ctx = np.zeros(64, np.float32)
    for i in range(0, len(pcm) - 512 + 1, 512):
        frame = pcm[i : i + 512].clip(-1, 1)
        yield np.concatenate([ctx, frame]).reshape(1, 576).astype(np.float32)
        ctx = frame[-64:]


def main(paths):
    s = rt.InferenceSession(str(ONNX), providers=["CPUExecutionProvider"])
    mlm = ct.models.MLModel(str(MLPKG), compute_units=ct.ComputeUnit.CPU_ONLY)
    total, maxd = 0, 0.0
    dis = {t: 0 for t in THRESH}
    for path in paths:
        with wave.open(path) as w:
            assert w.getframerate() == 16000 and w.getnchannels() == 1, path
            pcm = np.frombuffer(w.readframes(w.getnframes()), np.int16).astype(np.float32) / 32768.0
        st_o = np.zeros((2, 1, 128), np.float32)
        st_c = np.zeros((2, 1, 128), np.float32)
        for x in frames(pcm):
            p_o, st_o = s.run(None, {"input": x, "state": st_o, "sr": np.array(16000, np.int64)})
            r = mlm.predict({"input": x, "state_in": st_c})
            st_c = np.asarray(r["state_out"]).reshape(2, 1, 128).astype(np.float32)
            po, pc = float(p_o[0, 0]), float(np.asarray(r["output"]).reshape(-1)[0])
            maxd = max(maxd, abs(po - pc))
            for t in THRESH:
                dis[t] += (po >= t) != (pc >= t)
            total += 1
        print(f"{path}: done")
    print(f"TOTAL frames={total} max|dProb|={maxd:.3e} disagreements={dis}")
    assert all(v == 0 for v in dis.values()), "decision divergence"
    print("PARITY OK")


if __name__ == "__main__":
    args = sys.argv[1:] or sorted(
        glob.glob(str(HERE.parents[3] / "adaptive/win_raws/*_raw.wav"))
    )
    main(args)
