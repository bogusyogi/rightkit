#!/usr/bin/env python
"""Inventory and stage real near-phonetic negatives for Zephyr."""

from __future__ import annotations

import argparse
import csv
import json
import re
import shutil
from collections import defaultdict
from pathlib import Path


TERMS = ["sifr", "suffer", "zipper", "zapper", "zepto", "safar", "zapp"]


def stable_slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")[:80] or "row"


def copy_cv_hits(cv_root: Path, output_root: Path, cap_per_term: int) -> list[dict]:
    validated = cv_root / "validated.tsv"
    clips = cv_root / "clips"
    if not validated.exists() or not clips.exists():
        return []

    patterns = {term: re.compile(rf"\b{re.escape(term)}\b", re.IGNORECASE) for term in TERMS}
    counts: dict[str, int] = defaultdict(int)
    rows: list[dict] = []
    with validated.open("r", encoding="utf-8", errors="replace", newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for src in reader:
            sentence = src.get("sentence", "")
            matches = [term for term, pattern in patterns.items() if pattern.search(sentence)]
            if not matches:
                continue
            clip_name = src.get("path", "")
            src_path = clips / clip_name
            if not src_path.exists():
                continue
            for term in matches:
                if counts[term] >= cap_per_term:
                    continue
                counts[term] += 1
                dst_dir = output_root / "raw" / "near_phonetic_real" / "common_voice" / term
                dst_dir.mkdir(parents=True, exist_ok=True)
                dst_path = dst_dir / f"{counts[term]:04d}_{stable_slug(sentence)}{src_path.suffix.lower()}"
                if not dst_path.exists():
                    shutil.copy2(src_path, dst_path)
                rows.append(
                    {
                        "schema_version": 1,
                        "term": term,
                        "sentence": sentence,
                        "source": "common_voice_v25",
                        "source_path": str(src_path),
                        "path": str(dst_path),
                        "label": 0,
                        "word": "",
                        "semantic_bucket": "near_phonetic",
                        "source_bucket": "common_voice_near_phonetic",
                        "false_fire_type": "phonetic_near_match_candidate",
                    }
                )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expansion-root", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607"))
    parser.add_argument("--cv-root", type=Path, default=Path(r"D:\Claude\heardright\train\pico\CV\cv-corpus-25.0-2026-03-09\en"))
    parser.add_argument("--cap-per-term", type=int, default=50)
    args = parser.parse_args()

    rows = copy_cv_hits(args.cv_root, args.expansion_root, args.cap_per_term)
    manifest = args.expansion_root / "manifests" / "near_phonetic_real_manifest.jsonl"
    report = args.expansion_root / "reports" / "near_phonetic_inventory.json"
    manifest.parent.mkdir(parents=True, exist_ok=True)
    with manifest.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    counts = defaultdict(int)
    for row in rows:
        counts[row["term"]] += 1
    summary = {
        "terms": TERMS,
        "rows_staged": len(rows),
        "counts_by_term": {term: counts[term] for term in TERMS},
        "manifest": str(manifest),
    }
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
