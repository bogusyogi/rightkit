"""Whisper bucketed dictation-WER eval (CT2/CUDA + whisper.cpp Vulkan) over ANY
manifest with ground truth. Companion to bench_whisper_commands.py (which is
command-pack specific). Reports overall + by-group WER with optional
--initial-prompt, so Whisper can be scored head-to-head with Parakeet on the
same v2/global/accent buckets.

Example:
  py -3.11 whisper_bucket_eval.py --ct2 large-v3-turbo --ct2 distil-large-v3 \
    --vulkan turbo-q8=app/.../ggml-large-v3-turbo-q8_0.bin \
    --initial-prompt "Zephyr stop. Zephyr send." \
    --manifest tests/asr_global_eval/manifest.json \
    --clips-dir tests/asr_global_eval/clips --out .cache/asr-eval/whisper_global.json
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import time
from collections import defaultdict
from pathlib import Path

import jiwer

WCPP_BIN = Path(r"D:\Claude\heardright\.wcpp_vulkan\whisper.cpp\build_cl\bin\Release\whisper-cli.exe")
TS_RE = re.compile(r"^\s*\[[0-9:.]+\s*-->\s*[0-9:.]+\]\s*")


def norm(text: str) -> str:
    return " ".join(
        "".join(c if (c.isascii() and (c.isalnum() or c in " '")) else " " for c in text.lower()).split()
    )


def manifest_items(manifest: str, clips_dir: str):
    for it in json.loads(Path(manifest).read_text(encoding="utf-8")):
        p = it.get("path")
        if p:
            pp = Path(p)
            wav = pp if pp.is_absolute() else Path(clips_dir) / pp
        else:
            wav = Path(clips_dir) / it["clip"]
        yield it, wav


def by_group(rows: list[dict]) -> dict:
    g: dict[str, list[float]] = defaultdict(list)
    for r in rows:
        g[r["group"]].append(r["wer"])
    return {k: round(sum(v) / len(v) * 100, 2) for k, v in sorted(g.items())}


def summarize(engine: str, size_mb: float | None, rows: list[dict]) -> dict:
    return {
        "engine": engine,
        "model_size_mb": round(size_mb, 1) if size_mb else None,
        "clips": len(rows),
        "overall_wer": round(sum(r["wer"] for r in rows) / len(rows) * 100, 2),
        "mean_decode_ms": round(sum(r["decode_ms"] for r in rows) / len(rows), 1),
        "by_group_wer": by_group(rows),
        "rows": rows,
    }


def run_ct2(model: str, manifest: str, clips_dir: str, beam: int, prompt: str | None) -> dict:
    from faster_whisper import WhisperModel

    wm = WhisperModel(model, device="cuda", compute_type="float16")
    rows = []
    for it, wav in manifest_items(manifest, clips_dir):
        t0 = time.perf_counter()
        try:
            segs, _ = wm.transcribe(
                str(wav), beam_size=beam, language="en",
                condition_on_previous_text=False, vad_filter=False, initial_prompt=prompt,
            )
            txt = " ".join(s.text for s in segs).strip()
        except Exception as exc:
            txt = ""
            print(f"  CLIP-FAIL {it.get('clip')}: {exc}", flush=True)
        rows.append({"clip": it.get("clip"), "group": it["group"],
                     "wer": jiwer.wer(norm(it["gt"]), norm(txt)),
                     "decode_ms": round((time.perf_counter() - t0) * 1000, 1), "text": txt})
    return summarize(f"ct2-{model}-b{beam}" + ("-ip" if prompt else ""), None, rows)


def run_vulkan(name: str, path: Path, manifest: str, clips_dir: str, prompt: str | None) -> dict:
    flags = os.environ.get("WCPP_FLAGS", "-bs 1 -bo 1").split()
    rows = []
    for it, wav in manifest_items(manifest, clips_dir):
        cmd = [str(WCPP_BIN), "-m", str(path), "-f", str(wav), "-l", "en", *flags]
        if prompt:
            cmd += ["--prompt", prompt]
        t0 = time.perf_counter()
        pr = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", check=False)
        lines = [ln[mt.end():].strip() for ln in pr.stdout.splitlines() if (mt := TS_RE.match(ln))]
        txt = " ".join(lines).strip()
        rows.append({"clip": it.get("clip"), "group": it["group"],
                     "wer": jiwer.wer(norm(it["gt"]), norm(txt)),
                     "decode_ms": round((time.perf_counter() - t0) * 1000, 1), "text": txt})
    return summarize(f"vulkan-{name}" + ("-ip" if prompt else ""), path.stat().st_size / 1024 / 1024, rows)


def parse_vulkan(v: str):
    n, p = v.split("=", 1)
    return n, Path(p)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ct2", action="append", default=[])
    ap.add_argument("--vulkan", type=parse_vulkan, action="append", default=[])
    ap.add_argument("--beam", type=int, default=1)
    ap.add_argument("--initial-prompt", default=None)
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--clips-dir", required=True)
    ap.add_argument("--out", type=Path, required=True)
    a = ap.parse_args()

    res = [run_ct2(m, a.manifest, a.clips_dir, a.beam, a.initial_prompt) for m in a.ct2]
    res += [run_vulkan(n, p, a.manifest, a.clips_dir, a.initial_prompt) for n, p in a.vulkan]
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps(res, indent=2, ensure_ascii=False), encoding="utf-8")
    for r in res:
        print(f"{r['engine']}: WER {r['overall_wer']}% mean {r['mean_decode_ms']}ms size {r['model_size_mb']}  by-group {r['by_group_wer']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
