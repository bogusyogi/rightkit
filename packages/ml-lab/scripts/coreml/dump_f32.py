#!/usr/bin/env python3
"""Load an audio file -> 16 kHz mono f32, write raw little-endian f32 to stdout.
Usage: dump_f32.py <audio-file>   (pipe into the whisper_diff Rust example)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pico_eval import load_audio_16k_mono  # noqa: E402

a = load_audio_16k_mono(sys.argv[1])
sys.stdout.buffer.write(a.astype("<f4").tobytes())
