#!/usr/bin/env bash
# Whisper -> CoreML/ANE (full encoder + decoder) via whisperkittools.
#
# RUNS ON: Linux or WSL2 (same environment as the Parakeet conversion).
#   env: pip install whisperkittools   (Argmax open tooling, MIT)
#
# WHEN YOU NEED THIS: only for a CUSTOM quantization variant or a fine-tune.
# For STOCK large-v3-turbo the converted CoreML model is already published open
# (argmaxinc/whisperkit-coreml) -- skip conversion and benchmark it straight on
# the Mac (it downloads the open model).
#
# This is the PARITY path (full enc+dec on ANE), the Whisper analog of our
# Parakeet conversion -- NOT whisper.cpp -DWHISPER_COREML (encoder-only).
set -euo pipefail

MODEL="${1:-openai/whisper-large-v3-turbo}"
OUT="${2:-./out/whisper_lv3turbo}"

echo "[whisper-convert] model=$MODEL out=$OUT"
# VERIFY the exact flags against `whisperkit-generate-model --help` on your
# installed whisperkittools version (the CLI moved under argmax-oss-swift in 2026).
whisperkit-generate-model \
  --model-version "$MODEL" \
  --output-dir "$OUT"

# LEVER #2 (palettization) for Whisper: whisperkittools exposes per-encoder /
# per-decoder quantization (mixed-bit palettization) -- sweep it like Parakeet §3.
# e.g. add `--quantize <recipe>` once you've confirmed the flag name with --help.

echo "[done] copy $OUT to the Mac and benchmark it there"
