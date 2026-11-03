#!/usr/bin/env python3
"""Record mic to a clean 16 kHz mono PCM16 WAV (no compression).
Usage: record.py [out.wav]   — speak, then press Ctrl-C to stop.
Default out: ~/Desktop/hr_rec.wav
"""
import os
import sys

import numpy as np
import sounddevice as sd
import soundfile as sf

SR = 16_000
out = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/Desktop/hr_rec.wav")

print(f"Recording to {out}  (16 kHz mono)\n>>> speak now — press Ctrl-C to stop <<<", flush=True)
frames = []
try:
    with sd.InputStream(samplerate=SR, channels=1, dtype="float32") as stream:
        while True:
            data, _ = stream.read(SR // 8)
            frames.append(data.copy())
except KeyboardInterrupt:
    pass

if not frames:
    print("no audio captured", file=sys.stderr)
    sys.exit(1)
audio = np.concatenate(frames).reshape(-1)
sf.write(out, audio, SR, subtype="PCM_16")
peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
print(f"\nsaved {out}  ({len(audio) / SR:.1f}s, peak {peak:.2f})")
