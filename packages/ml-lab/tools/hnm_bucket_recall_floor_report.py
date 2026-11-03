#!/usr/bin/env python3
"""Recall-floor bucket report for score-bearing HNM eval JSONLs.

This is intentionally path-based because canon31559 eval rows use Windows-style
clip paths that are not always present in the remote manifest domain map.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np


def import_eval_helpers():
    for candidate in (Path.cwd(), Path(__file__).resolve().parent.parent):
        if (candidate / "cascade_eval.py").exists():
            sys.path.insert(0, str(candidate))
            break
    from cascade_eval import fires_at, wilson_lower_bound

    return fires_at, wilson_lower_bound


def bucket_for(path: str) -> str:
    p = path.replace("\\", "/").lower()
    if "/audio/pos/" in p or p.endswith("/pos"):
        return "pos"
    if "mic_capture" in p or "adrian_mic" in p or "/adrian" in p:
        return "adrian_mic"
    if "gigaspeech" in p:
        return "gigaspeech"
    if "common_voice" in p or "commonvoice" in p or "/cv_" in p or "/cv/" in p:
        return "common_voice"
    if "music" in p or "musan" in p:
        return "music"
    return "other"


def theta_grid() -> list[float]:
    vals = (
        list(np.arange(0.50, 0.99, 0.01))
        + list(np.arange(0.99, 0.9999, 0.0005))
        + list(np.arange(0.9999, 0.999991, 0.00001))
        + [1.0]
    )
    return sorted({round(float(x), 6) for x in vals})


def fmt(v: float | None) -> str:
    if v is None:
        return ""
    return f"{v:.6f}" if v < 1 else f"{v:.3f}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--jsonl", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--family", default="")
    ap.add_argument("--out-tsv", required=True)
    ap.add_argument("--out-md", required=True)
    ap.add_argument("--expected-rows", type=int, default=31559)
    ap.add_argument("--recall-wlo-floor", type=float, default=0.985)
    ap.add_argument("--blended-cap", type=float, default=0.5)
    args = ap.parse_args()

    fires_at, wilson_lower_bound = import_eval_helpers()
    path = Path(args.jsonl)
    rows = 0
    pos_scores: list[list[float]] = []
    neg_by_bucket: dict[str, list[tuple[list[float], float]]] = defaultdict(list)
    no_scores = 0

    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rows += 1
            r = json.loads(line)
            scores = r.get("scores")
            if scores is None:
                no_scores += 1
                continue
            if r.get("dom") == "POS":
                pos_scores.append(scores)
            else:
                b = bucket_for(str(r.get("clip", "")))
                neg_by_bucket[b].append((scores, float(r.get("hours", 0.0))))

    if rows != args.expected_rows:
        raise SystemExit(f"row count {rows} != expected {args.expected_rows}")
    if no_scores:
        raise SystemExit(f"{no_scores} rows lacked scores")

    best = None
    for th in theta_grid():
        hits = sum(1 for sc in pos_scores if fires_at(sc, th) > 0)
        wlo = wilson_lower_bound(hits, len(pos_scores)) if pos_scores else 0.0
        if wlo >= args.recall_wlo_floor:
            best = (th, hits, hits / max(1, len(pos_scores)), wlo)
    if best is None:
        th = theta_grid()[0]
        hits = sum(1 for sc in pos_scores if fires_at(sc, th) > 0)
        wlo = wilson_lower_bound(hits, len(pos_scores)) if pos_scores else 0.0
        best = (th, hits, hits / max(1, len(pos_scores)), wlo)

    theta, hits, recall, wlo = best
    buckets = ["adrian_mic", "gigaspeech", "common_voice", "music", "other"]
    fphr: dict[str, float] = {}
    for b in buckets:
        fires = 0.0
        hours = 0.0
        for scores, h in neg_by_bucket.get(b, []):
            fires += fires_at(scores, theta)
            hours += h
        fphr[b] = fires / hours if hours > 0 else 0.0

    blended = sum(fphr[b] for b in buckets[:4]) / 4.0
    policy_ok = wlo >= args.recall_wlo_floor and blended <= args.blended_cap
    verdict = "PASS" if policy_ok else "FAIL"

    header = [
        "family",
        "label",
        "rows",
        "theta",
        "recall",
        "wlo",
        "pos_hits",
        "pos_total",
        "adrian_mic",
        "gigaspeech",
        "common_voice",
        "music",
        "other",
        "blended",
        "policy_ok",
        "verdict",
    ]
    row = [
        args.family,
        args.label,
        str(rows),
        fmt(theta),
        fmt(recall),
        fmt(wlo),
        str(hits),
        str(len(pos_scores)),
        *(fmt(fphr[b]) for b in buckets),
        fmt(blended),
        str(policy_ok),
        verdict,
    ]

    Path(args.out_tsv).write_text("\t".join(header) + "\n" + "\t".join(row) + "\n", encoding="utf-8")
    md = [
        f"# {args.label} recall-floor bucket report",
        "",
        f"- Rows: {rows}",
        f"- Theta*: {fmt(theta)}",
        f"- Recall: {fmt(recall)} ({hits}/{len(pos_scores)})",
        f"- Wilson lower: {fmt(wlo)}",
        f"- Blended FP/hr: {fmt(blended)}",
        f"- Verdict: {verdict}",
        "",
        "| Bucket | FP/hr |",
        "|---|---:|",
    ]
    md.extend(f"| {b} | {fmt(fphr[b])} |" for b in buckets)
    Path(args.out_md).write_text("\n".join(md) + "\n", encoding="utf-8")
    print("\t".join(header))
    print("\t".join(row))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
