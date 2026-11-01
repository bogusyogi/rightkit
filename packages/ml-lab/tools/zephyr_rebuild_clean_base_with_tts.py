from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def read_jsonl(path: Path):
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                yield json.loads(line)


def write_jsonl(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, separators=(",", ":")) + "\n")


def force_split(row: dict, split: str) -> dict:
    out = dict(row)
    out["split"] = split
    return out


def summarize(rows: list[dict]) -> list[dict]:
    counts: Counter[tuple[str, int, str, str]] = Counter()
    for row in rows:
        counts[
            (
                str(row.get("split") or ""),
                int(row.get("label") or 0),
                str(row.get("source_bucket") or ""),
                str(row.get("semantic_bucket") or ""),
            )
        ] += 1
    return [
        {
            "split": split,
            "label": label,
            "source_bucket": source_bucket,
            "semantic_bucket": semantic_bucket,
            "rows": row_count,
        }
        for (split, label, source_bucket, semantic_bucket), row_count in sorted(counts.items())
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-manifest", type=Path, required=True)
    parser.add_argument("--train-tts-manifest", type=Path, action="append", default=[])
    parser.add_argument("--eval-tts-manifest", type=Path, action="append", default=[])
    parser.add_argument("--out-train-manifest", type=Path, required=True)
    parser.add_argument("--out-eval-manifest", type=Path, required=True)
    parser.add_argument("--summary-out", type=Path, default=None)
    parser.add_argument("--base-eval-split", default="eval_primary")
    parser.add_argument("--train-split", default="train")
    parser.add_argument("--eval-split", default="eval_primary")
    args = parser.parse_args()

    base_rows = list(read_jsonl(args.base_manifest))
    base_train_rows = [row for row in base_rows if str(row.get("split") or "") == args.train_split]
    base_eval_neg_rows = [
        row
        for row in base_rows
        if str(row.get("split") or "") == args.base_eval_split and int(row.get("label") or 0) == 0
    ]

    train_tts_rows: list[dict] = []
    for manifest in args.train_tts_manifest:
        train_tts_rows.extend(force_split(row, args.train_split) for row in read_jsonl(manifest))

    eval_tts_rows: list[dict] = []
    for manifest in args.eval_tts_manifest:
        eval_tts_rows.extend(force_split(row, args.eval_split) for row in read_jsonl(manifest))

    out_train_rows = [*base_train_rows, *train_tts_rows]
    out_eval_rows = [*base_eval_neg_rows, *eval_tts_rows]

    write_jsonl(args.out_train_manifest, out_train_rows)
    write_jsonl(args.out_eval_manifest, out_eval_rows)

    summary = {
        "base_manifest": str(args.base_manifest),
        "out_train_manifest": str(args.out_train_manifest),
        "out_eval_manifest": str(args.out_eval_manifest),
        "base_train_rows": len(base_train_rows),
        "base_eval_negative_rows": len(base_eval_neg_rows),
        "train_tts_rows": len(train_tts_rows),
        "eval_tts_rows": len(eval_tts_rows),
        "out_train_rows": len(out_train_rows),
        "out_eval_rows": len(out_eval_rows),
        "train_summary": summarize(out_train_rows),
        "eval_summary": summarize(out_eval_rows),
    }
    summary_out = args.summary_out or args.out_train_manifest.with_suffix(args.out_train_manifest.suffix + ".summary.json")
    summary_out.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({"out_train_manifest": str(args.out_train_manifest), "out_eval_manifest": str(args.out_eval_manifest), "summary": str(summary_out)}, sort_keys=True))


if __name__ == "__main__":
    main()
