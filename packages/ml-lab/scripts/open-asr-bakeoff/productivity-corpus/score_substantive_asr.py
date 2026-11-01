from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Mapping, Sequence, TypedDict


ONES = {
    "zero": 0,
    "oh": 0,
    "one": 1,
    "two": 2,
    "three": 3,
    "four": 4,
    "five": 5,
    "six": 6,
    "seven": 7,
    "eight": 8,
    "nine": 9,
    "ten": 10,
    "eleven": 11,
    "twelve": 12,
    "thirteen": 13,
    "fourteen": 14,
    "fifteen": 15,
    "sixteen": 16,
    "seventeen": 17,
    "eighteen": 18,
    "nineteen": 19,
}
TENS = {
    "twenty": 20,
    "thirty": 30,
    "forty": 40,
    "fifty": 50,
    "sixty": 60,
    "seventy": 70,
    "eighty": 80,
    "ninety": 90,
}
ORDINALS = {
    "first": 1,
    "second": 2,
    "third": 3,
    "fourth": 4,
    "fifth": 5,
    "sixth": 6,
    "seventh": 7,
    "eighth": 8,
    "ninth": 9,
    "tenth": 10,
    "eleventh": 11,
    "twelfth": 12,
    "thirteenth": 13,
    "fourteenth": 14,
    "fifteenth": 15,
    "sixteenth": 16,
    "seventeenth": 17,
    "eighteenth": 18,
    "nineteenth": 19,
    "twentieth": 20,
    "thirtieth": 30,
    "fortieth": 40,
    "fiftieth": 50,
    "sixtieth": 60,
    "seventieth": 70,
    "eightieth": 80,
    "ninetieth": 90,
}
SCALES = {"hundred": 100, "thousand": 1_000, "million": 1_000_000}
NUMBER_WORDS = set(ONES) | set(TENS) | set(ORDINALS) | set(SCALES) | {"and"}
CURRENCY_WORDS = {"dollar", "dollars", "usd"}


class ContentComparison(TypedDict):
    exact: bool
    reference_canonical: str
    hypothesis_canonical: str
    content_error_count: int


def _parse_cardinal(words: list[str]) -> int:
    total = 0
    current = 0
    for word in words:
        if word == "and":
            continue
        if word in ONES:
            current += ONES[word]
        elif word in TENS:
            current += TENS[word]
        elif word in ORDINALS:
            current += ORDINALS[word]
        elif word == "hundred":
            current = max(1, current) * 100
        elif word in ("thousand", "million"):
            total += max(1, current) * SCALES[word]
            current = 0
        else:
            raise ValueError(f"not a number word: {word}")
    return total + current


def _parse_year_style(words: list[str]) -> int | None:
    if len(words) < 2 or any(word in SCALES or word in ORDINALS for word in words):
        return None
    first = _parse_cardinal([words[0]])
    rest = _parse_cardinal(words[1:])
    if 10 <= first <= 99 and 0 <= rest <= 99:
        return int(f"{first}{rest:02d}")
    return None


def _canonicalize_number_words(words: list[str]) -> list[str]:
    ordinal_positions = [index for index, word in enumerate(words) if word in ORDINALS]
    if ordinal_positions:
        boundary = ordinal_positions[0] + 1
        values = [str(_parse_cardinal(words[:boundary]))]
        if boundary < len(words):
            remainder = words[boundary:]
            year = _parse_year_style(remainder)
            values.append(str(year if year is not None else _parse_cardinal(remainder)))
        return values
    year = _parse_year_style(words)
    return [str(year if year is not None else _parse_cardinal(words))]


def canonicalize_content(text: str) -> str:
    tokens = re.findall(r"[a-z]+(?:'[a-z]+)?|\d+", str(text).lower())
    collapsed: list[str] = []
    index = 0
    while index < len(tokens):
        if index + 1 < len(tokens) and tokens[index] in {"a", "p"} and tokens[index + 1] == "m":
            collapsed.append(tokens[index] + "m")
            index += 2
        elif tokens[index] in CURRENCY_WORDS:
            index += 1
        else:
            collapsed.append(tokens[index])
            index += 1
    tokens = collapsed

    canonical: list[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token.isdigit():
            if (
                index + 1 < len(tokens)
                and tokens[index + 1].isdigit()
                and len(tokens[index + 1]) == 3
                and (index + 2 >= len(tokens) or tokens[index + 2] not in {"am", "pm"})
            ):
                canonical.append(token + tokens[index + 1])
                index += 2
            else:
                canonical.append(str(int(token)))
                index += 1
            continue
        if token not in NUMBER_WORDS:
            canonical.append(token)
            index += 1
            continue

        end = index
        while end < len(tokens) and tokens[end] in NUMBER_WORDS:
            end += 1
        number_words = tokens[index:end]
        if end < len(tokens) and tokens[end] in {"am", "pm"} and len(number_words) >= 2:
            hour = _parse_cardinal(number_words[:1])
            minute = _parse_cardinal(number_words[1:])
            canonical.extend((str(hour), str(minute)))
        else:
            canonical.extend(_canonicalize_number_words(number_words))
        index = end
    return " ".join(canonical)


def _word_distance(reference: str, hypothesis: str) -> int:
    ref = reference.split()
    hyp = hypothesis.split()
    previous = list(range(len(hyp) + 1))
    for ref_word in ref:
        current = [previous[0] + 1]
        for column, hyp_word in enumerate(hyp, start=1):
            current.append(
                min(
                    previous[column] + 1,
                    current[column - 1] + 1,
                    previous[column - 1] + (ref_word != hyp_word),
                )
            )
        previous = current
    return previous[-1]


def compare_content(reference: str, hypothesis: str) -> ContentComparison:
    reference_canonical = canonicalize_content(reference)
    hypothesis_canonical = canonicalize_content(hypothesis)
    content_error_count = _word_distance(reference_canonical, hypothesis_canonical)
    return {
        "exact": content_error_count == 0,
        "reference_canonical": reference_canonical,
        "hypothesis_canonical": hypothesis_canonical,
        "content_error_count": content_error_count,
    }


def score_hypotheses(
    truth_rows: Sequence[Mapping[str, str]] | Mapping[str, Mapping[str, str]],
    hypotheses_by_model: Mapping[str, Mapping[str, str]],
) -> dict[str, object]:
    if isinstance(truth_rows, Mapping):
        normalized_truth_rows = [
            {
                **truth,
                "id": truth.get("id", truth.get("clip_id", clip_id)),
                "transcription": truth.get(
                    "transcription", truth.get("ground_truth", "")
                ),
            }
            for clip_id, truth in truth_rows.items()
        ]
    else:
        normalized_truth_rows = list(truth_rows)
    summaries = {
        model: {
            "clips": len(normalized_truth_rows),
            "problem_clips": 0,
            "content_error_count": 0,
        }
        for model in hypotheses_by_model
    }
    differences: list[dict[str, object]] = []
    for truth in normalized_truth_rows:
        clip_id = truth["id"]
        reference = truth["transcription"]
        model_rows: dict[str, dict[str, object]] = {}
        has_problem = False
        for model, hypotheses in hypotheses_by_model.items():
            hypothesis = hypotheses.get(clip_id, "")
            comparison = compare_content(reference, hypothesis)
            summaries[model]["content_error_count"] += comparison["content_error_count"]
            if not comparison["exact"]:
                summaries[model]["problem_clips"] += 1
                has_problem = True
            model_rows[model] = {
                "exact": comparison["exact"],
                "hypothesis": hypothesis,
                "hypothesis_canonical": comparison["hypothesis_canonical"],
                "content_error_count": comparison["content_error_count"],
            }
        if has_problem:
            differences.append(
                {
                    "id": clip_id,
                    "reference": reference,
                    "reference_canonical": canonicalize_content(reference),
                    "models": model_rows,
                }
            )
    return {"models": summaries, "differences": differences}


def _markdown_text(value: object) -> str:
    return str(value).replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def render_markdown(report: Mapping[str, object]) -> str:
    models = report["models"]
    differences = report["differences"]
    lines = [
        "# Substantive ASR differences",
        "",
        "Formatting-only differences are omitted. This includes casing, punctuation, number/date/time rendering, currency rendering, and AM/PM spacing.",
        "",
        "| Model | Clips | Problem clips | Content words wrong |",
        "|---|---:|---:|---:|",
    ]
    for model, summary in models.items():
        lines.append(
            f"| {_markdown_text(model)} | {summary['clips']} | "
            f"{summary['problem_clips']} | {summary['content_error_count']} |"
        )
    for row in differences:
        lines.extend(("", f"## {_markdown_text(row['id'])}", ""))
        lines.append(f"Reference: {_markdown_text(row['reference'])}")
        lines.append("")
        for model, model_row in row["models"].items():
            if model_row["exact"]:
                lines.append(f"- {_markdown_text(model)}: Exact")
            else:
                count = model_row["content_error_count"]
                label = "content word wrong" if count == 1 else "content words wrong"
                lines.append(
                    f"- {_markdown_text(model)}: {_markdown_text(model_row['hypothesis'])} "
                    f"({count} {label})"
                )
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Report only substantive ASR content differences."
    )
    parser.add_argument("--truth", required=True, type=Path)
    parser.add_argument(
        "--hypothesis",
        action="append",
        required=True,
        metavar="MODEL=JSONL",
        help="Repeat once per model; JSONL rows require id and hypothesis.",
    )
    parser.add_argument("--json-out", required=True, type=Path)
    parser.add_argument("--md-out", required=True, type=Path)
    args = parser.parse_args()

    truth_rows = json.loads(args.truth.read_text(encoding="utf-8"))
    hypotheses_by_model: dict[str, dict[str, str]] = {}
    for specification in args.hypothesis:
        if "=" not in specification:
            parser.error("--hypothesis must be MODEL=JSONL")
        model, filename = specification.split("=", 1)
        rows: dict[str, str] = {}
        for line in Path(filename).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            row = json.loads(line)
            rows[str(row["id"])] = str(row.get("hypothesis", ""))
        hypotheses_by_model[model] = rows

    report = score_hypotheses(truth_rows, hypotheses_by_model)
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.md_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    args.md_out.write_text(render_markdown(report), encoding="utf-8")


if __name__ == "__main__":
    main()
