"""Verify synthesised dictation clips actually contain their script.

For the TTS long-form group the script IS the ground truth, so a synthesis failure
does not show up as bad audio — it shows up as a corpus whose reference is wrong.
If Chatterbox drops, repeats, or garbles a sentence, every future engine gets
charged for words that were never spoken.

This scores an ASR hypothesis set against the scripts. It does not run ASR itself:
it consumes `hypotheses.jsonl` from the pinned bakeoff adapters, so the verifier is
the same code path the bakeoff already trusts, on the same conditioned bytes.

VERIFIER CHOICE
    Use **cohere-transcribe** (Adrian, 2026-07-20). It is the most accurate engine
    measured on this corpus — 4.94% against Unified's 7.49% — so the fewest of its
    disagreements with the script are its own errors. Do NOT use the production
    engine as the sole verifier here: this corpus exists to *compare* engines, and
    gating it on the shipping model biases the set toward what that model already
    handles. Production-model agreement is a useful second opinion, not the gate.

    Generate hypotheses first:

      .venv-infer/Scripts/python.exe adapters/cohere-transcribe/adapter.py \
          --manifest <filtered manifest of the TTS clips> \
          --out-dir <out> --full --model-dir <cohere model dir>

    then score:

      py -3.11 verify_tts_intelligibility.py \
          --canonical-dir ../canonical-eval --hypotheses <out>/hypotheses.jsonl

TWO GATES, AND WHY WER ALONE IS NOT ENOUGH
    A whole dropped sentence barely moves WER on a 750-word script. The first pass
    of this corpus lost "So we rebuilt it." from `product_announcement_draft` and
    still scored 2.83% — comfortably inside any WER threshold, while the reference
    claimed four words that were never spoken. So the deletion-run gate is the one
    that actually protects ground truth:

    * `--max-wer`          catches broad garbling (default 0.15)
    * `--max-deletion-run` catches dropped phrases: the longest run of consecutive
                           script words absent from the hypothesis (default 2)

    A real dropout is confirmed by *consensus* — run a second adapter and check the
    same run is missing. A single model missing 3 words may just be that model.
"""

from __future__ import annotations

import argparse
import difflib
import json
import re
import sys
from pathlib import Path

TTS_LONG_FORM_GROUP = "dictation_tts_long_form"

NUMBER_WORDS = {
    "0": "zero", "1": "one", "2": "two", "3": "three", "4": "four", "5": "five",
    "6": "six", "7": "seven", "8": "eight", "9": "nine", "10": "ten",
    "11": "eleven", "12": "twelve", "15": "fifteen", "20": "twenty",
    "30": "thirty", "40": "forty", "50": "fifty", "60": "sixty", "70": "seventy",
    "80": "eighty", "90": "ninety", "100": "hundred", "300": "three hundred",
    "000": "thousand",
}


def normalize(text: str) -> list[str]:
    text = text.lower().replace("%", " percent ")
    text = re.sub(r"[^a-z0-9\s]", " ", text)
    tokens: list[str] = []
    for token in text.split():
        tokens.extend(NUMBER_WORDS.get(token, token).split())
    return tokens


def wer(reference: list[str], hypothesis: list[str]) -> tuple[float, int, int, int]:
    rows, cols = len(reference) + 1, len(hypothesis) + 1
    distance = [[0] * cols for _ in range(rows)]
    for i in range(rows):
        distance[i][0] = i
    for j in range(cols):
        distance[0][j] = j
    for i in range(1, rows):
        for j in range(1, cols):
            if reference[i - 1] == hypothesis[j - 1]:
                distance[i][j] = distance[i - 1][j - 1]
            else:
                distance[i][j] = 1 + min(
                    distance[i - 1][j - 1], distance[i - 1][j], distance[i][j - 1]
                )
    i, j = len(reference), len(hypothesis)
    subs = dels = ins = 0
    while i > 0 or j > 0:
        if i > 0 and j > 0 and reference[i - 1] == hypothesis[j - 1]:
            i, j = i - 1, j - 1
        elif i > 0 and j > 0 and distance[i][j] == distance[i - 1][j - 1] + 1:
            subs += 1
            i, j = i - 1, j - 1
        elif i > 0 and distance[i][j] == distance[i - 1][j] + 1:
            dels += 1
            i -= 1
        else:
            ins += 1
            j -= 1
    return distance[len(reference)][len(hypothesis)] / max(1, len(reference)), subs, dels, ins


def deletion_runs(reference: list[str], hypothesis: list[str]) -> list[str]:
    """Contiguous script phrases missing from the hypothesis, longest first."""
    runs = []
    for op, i1, i2, _j1, _j2 in difflib.SequenceMatcher(
        a=reference, b=hypothesis, autojunk=False
    ).get_opcodes():
        if op == "delete":
            runs.append(" ".join(reference[i1:i2]))
    return sorted(runs, key=lambda run: -len(run.split()))


def load_hypotheses(path: Path) -> dict[str, str]:
    hypotheses: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        clip_id = row.get("id") or row.get("clip_id")
        text = row.get("hyp") or row.get("text") or row.get("hypothesis") or ""
        hypotheses[clip_id] = text
    return hypotheses


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--canonical-dir", required=True, type=Path)
    parser.add_argument("--hypotheses", required=True, type=Path)
    parser.add_argument("--max-wer", type=float, default=0.15)
    parser.add_argument("--max-deletion-run", type=int, default=2)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    truth = json.loads(
        (args.canonical_dir / "_scorer_private" / "id_to_truth.json").read_text(
            encoding="utf-8"
        )
    )
    hypotheses = load_hypotheses(args.hypotheses)

    rows = []
    failures = []
    scored = {
        clip_id: row
        for clip_id, row in truth.items()
        if row["group"] == TTS_LONG_FORM_GROUP and clip_id in hypotheses
    }
    if not scored:
        raise SystemExit("no TTS clips found in both the corpus and the hypotheses")

    for clip_id, row in sorted(scored.items(), key=lambda kv: kv[1]["duration_s"]):
        reference = normalize(row["ground_truth"])
        hypothesis = normalize(hypotheses[clip_id])
        rate, subs, dels, ins = wer(reference, hypothesis)
        runs = deletion_runs(reference, hypothesis)
        longest = len(runs[0].split()) if runs else 0
        bad_wer = rate > args.max_wer
        bad_run = longest > args.max_deletion_run
        if bad_wer or bad_run:
            failures.append((clip_id, rate, runs[:3]))
        rows.append(
            {
                "clip_id": clip_id,
                "duration_s": row["duration_s"],
                "ref_words": len(reference),
                "hyp_words": len(hypothesis),
                "wer": round(rate, 4),
                "sub": subs,
                "del": dels,
                "ins": ins,
                "longest_deletion_run": longest,
                "deletion_runs": runs[:5],
            }
        )
        flag = "FAIL" if (bad_wer or bad_run) else "ok  "
        print(
            f"{flag} {clip_id} {row['duration_s']:7.1f}s "
            f"ref={len(reference):4d} hyp={len(hypothesis):4d} "
            f"WER={rate:6.2%} longest_missing_run={longest}"
            + (f"  -> {runs[0]!r}" if longest > args.max_deletion_run else ""),
            flush=True,
        )

    if args.out:
        args.out.write_text(json.dumps(rows, indent=2) + "\n", encoding="utf-8")

    print(
        f"\nclips={len(rows)} "
        f"worst WER={max(r['wer'] for r in rows):.2%} (gate {args.max_wer:.0%}) "
        f"longest missing run={max(r['longest_deletion_run'] for r in rows)} "
        f"(gate {args.max_deletion_run})"
    )
    if failures:
        print(f"FAIL — {len(failures)} clip(s) diverge from their script:")
        for clip_id, rate, runs in failures:
            print(f"  {clip_id} WER={rate:.2%} missing={runs}")
        print("Confirm with a second adapter before regenerating: a run missing from")
        print("one model may be that model's error, not a synthesis dropout.")
        sys.exit(1)
    print("PASS — every clip faithfully contains its script")


if __name__ == "__main__":
    main()
