"""Exhaustive Whisper CT2 command-pack sweep — the Whisper-lane counterpart to
the RNNT/TDT variant sweep. Grid = model ladder x compute_type. One subprocess
per config (avoids multi-model VRAM OOM), resumable (skips existing outputs),
ranked by command-action pass then WER.

This is the broad CHEAP screen (13-clip command pack). Finalists then go to the
dictation buckets (whisper_bucket_eval.py) and the ggml/Vulkan quant sweep is a
separate wave.

Run: py -3.11 whisper_sweep.py
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCH = HERE / "bench_whisper_commands.py"
OUT = Path(r"D:/Claude/heardright/tauri-app-next/.cache/asr-eval/whisper_sweep")
OUT.mkdir(parents=True, exist_ok=True)
PROMPT = "Zephyr stop. Zephyr send. Zephyr enter."

# (label, faster-whisper model id or local CT2 path)
MODELS = [
    ("tiny.en", "tiny.en"),
    ("base.en", "base.en"),
    ("small.en", "small.en"),
    ("medium.en", "medium.en"),
    ("large-v3", "large-v3"),
    ("large-v3-turbo", "large-v3-turbo"),
    ("distil-large-v3", "distil-large-v3"),
    ("distil-large-v3.5", r"D:/Claude/heardright/app/heardright/models/ct2-distil-large-v3.5"),
    ("distil-medium.en", "distil-whisper/distil-medium.en"),
    ("distil-small.en", "distil-whisper/distil-small.en"),
]
COMPUTE = ["int8", "int8_float16", "float16"]

rows = []
fails = []
for label, mid in MODELS:
    for ct in COMPUTE:
        out = OUT / f"sweep_{label}_{ct}.json"
        if not out.exists():
            print(f">>> {label} / {ct}", flush=True)
            r = subprocess.run(
                [sys.executable, str(BENCH), "--ct2", mid, "--compute-type", ct,
                 "--beam", "1", "--initial-prompt", PROMPT, "--out", str(out)],
                capture_output=True, text=True,
            )
            if r.returncode != 0 or not out.exists():
                tail = (r.stderr.strip().splitlines() or ["no stderr"])[-1]
                print(f"    FAIL {label}/{ct}: {tail}", flush=True)
                fails.append((label, ct, tail))
                continue
        try:
            for v in json.load(open(out, encoding="utf-8")):
                rows.append((label, ct, v["command_action_pass"], v["command_action_total"],
                             v["overall_wer"], v["mean_decode_ms"]))
        except Exception as exc:
            print(f"    parse-fail {label}/{ct}: {exc}", flush=True)

print("\n=== WHISPER CT2 COMMAND SWEEP (beam1 +prompt) ===")
print(f"{'model':22}{'compute':14}{'cmd':>8}{'WER%':>8}{'dec_ms':>8}")
for label, ct, p, t, wer, dec in sorted(rows, key=lambda r: (-(r[2] / max(r[3], 1)), r[4])):
    print(f"{label:22}{ct:14}{p:>4}/{t:<3}{wer:>8}{dec:>8.0f}")
if fails:
    print(f"\nFAILED configs ({len(fails)}): " + ", ".join(f"{l}/{c}" for l, c, _ in fails))
print("=== WHISPER SWEEP DONE ===")
