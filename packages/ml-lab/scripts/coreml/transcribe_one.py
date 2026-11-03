#!/usr/bin/env python3
"""Transcribe ONE audio file with raw Parakeet CoreML (no cleanup).

Reuses pico_eval's Model (the ground-truth ASR that coreml_asr.rs ports).
Usage: transcribe_one.py <audio.wav> [--pkg <model_dir>]
Audio is loaded via soundfile (WAV/FLAC/OGG). For m4a, convert first:
  afconvert -f WAVE -d LEI16@16000 -c 1 in.m4a out.wav
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pico_eval import Model, load_audio_16k_mono  # noqa: E402

DEFAULT_PKG = os.path.expanduser(
    "~/claude/heardright/model_registry/coreml/parakeet-tdt-v3"
)


CONDITION_BIN = os.path.expanduser(
    "~/claude/heardright/tauri-app-next/heardright_core/target/debug/condition_audio"
)


def condition(audio, policy):
    """Pipe f32 mono samples through the real condition_audio bin (app parity)."""
    import struct
    import subprocess

    raw = struct.pack(f"<{len(audio)}f", *audio.tolist())
    out = subprocess.run(
        [CONDITION_BIN, "16000", policy], input=raw, capture_output=True
    ).stdout
    import numpy as np

    return np.frombuffer(out, dtype="<f4")


def main():
    args = sys.argv[1:]
    pkg = DEFAULT_PKG
    policy = None  # None = raw audio, no conditioning
    if "--pkg" in args:
        i = args.index("--pkg")
        pkg = args[i + 1]
        del args[i : i + 2]
    if "--condition" in args:
        i = args.index("--condition")
        policy = args[i + 1]
        del args[i : i + 2]
    if not args:
        print("usage: transcribe_one.py <audio.wav> [--condition <policy>] [--pkg <dir>]", file=sys.stderr)
        sys.exit(2)
    wav = args[0]
    audio = load_audio_16k_mono(wav)
    if policy:
        audio = condition(audio, policy)
    m = Model(pkg)
    print(m.transcribe(audio))


if __name__ == "__main__":
    main()
