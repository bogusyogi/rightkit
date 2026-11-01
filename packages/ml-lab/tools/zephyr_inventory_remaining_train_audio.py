#!/usr/bin/env python
"""Inventory remaining non-pico audio files under train.

Read-only helper for the Zephyr source-truth cleanup. It excludes the active
`pico` tree and the canonical conditioned packed store so the output is a raw
cleanup candidate list, not a whole-corpus scan result.
"""

from __future__ import annotations

import argparse
import json
import os
from collections import Counter, defaultdict
from pathlib import Path


AUDIO_EXTS = {
    ".wav",
    ".flac",
    ".mp3",
    ".m4a",
    ".ogg",
    ".opus",
    ".aac",
    ".wma",
    ".webm",
}


def is_under(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--train-root", type=Path, default=Path(r"D:\Claude\heardright\train"))
    parser.add_argument("--jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    args = parser.parse_args()

    train_root = args.train_root.resolve()
    excludes = [
        train_root / "pico",
        train_root / "conditioned" / "adaptive_hot_v2_5" / "_packed_shards",
    ]

    args.jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)

    counters = Counter()
    by_top: dict[str, Counter] = defaultdict(Counter)
    examples: dict[str, list[str]] = defaultdict(list)

    exclude_resolved = [root.resolve() for root in excludes if root.exists()]

    with args.jsonl.open("w", encoding="utf-8", newline="\n") as out:
        for dirpath, dirnames, filenames in os.walk(train_root):
            current = Path(dirpath).resolve()
            if any(current == root or is_under(current, root) for root in exclude_resolved):
                dirnames[:] = []
                continue
            kept_dirs = []
            for dirname in dirnames:
                child = (current / dirname).resolve()
                if any(child == root or is_under(child, root) for root in exclude_resolved):
                    counters["pruned_dirs"] += 1
                else:
                    kept_dirs.append(dirname)
            dirnames[:] = kept_dirs

            for filename in filenames:
                path = current / filename
                if path.suffix.lower() not in AUDIO_EXTS:
                    continue
                rel = path.relative_to(train_root)
                top = rel.parts[0] if rel.parts else ""
                size = path.stat().st_size
                row = {
                    "path": str(path),
                    "relative_path": str(rel),
                    "top": top,
                    "ext": path.suffix.lower(),
                    "bytes": size,
                }
                out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
                counters["audio_files"] += 1
                counters["audio_bytes"] += size
                by_top[top]["files"] += 1
                by_top[top]["bytes"] += size
                if len(examples[top]) < 10:
                    examples[top].append(str(rel))

    summary = {
        "train_root": str(train_root),
        "excluded_roots": [str(p) for p in excludes],
        "audio_files": counters["audio_files"],
        "audio_gb": round(counters["audio_bytes"] / (1024**3), 3),
        "pruned_dirs": counters["pruned_dirs"],
        "by_top": {
            top: {"files": counts["files"], "gb": round(counts["bytes"] / (1024**3), 3)}
            for top, counts in sorted(by_top.items())
        },
        "examples": dict(examples),
        "jsonl": str(args.jsonl),
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
