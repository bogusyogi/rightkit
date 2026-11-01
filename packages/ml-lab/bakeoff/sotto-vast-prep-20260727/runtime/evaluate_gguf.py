#!/usr/bin/env python3
"""Deterministic GGUF evaluation with accepted-gold row identity support."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path


def sha(path: str | Path) -> str:
    return "sha256:" + hashlib.sha256(Path(path).read_bytes()).hexdigest()


def canonical_row_id(row: dict[str, object]) -> str:
    """Use accepted-gold `row_id`, while accepting an identical legacy `id`."""
    row_id, legacy_id = row.get("row_id"), row.get("id")
    if row_id is not None and legacy_id is not None and row_id != legacy_id:
        raise ValueError("conflicting id and row_id identifiers")
    identity = row_id if row_id is not None else legacy_id
    if not isinstance(identity, str) or not identity:
        raise ValueError("evaluation row requires a non-empty string identifier (row_id or id)")
    if not isinstance(row.get("input_text"), str):
        raise ValueError("evaluation row requires string input_text")
    return identity


def _node_module(module: Path, export: str, row: dict[str, object]) -> dict[str, object]:
    """Call one named export of an ES module with `row` on stdin.

    The module path is passed as a file:// URL: Node's ESM loader rejects a bare
    Windows absolute path ("Received protocol 'd:'"), so `str(path)` fails on
    this platform.
    """
    program = f"const m=await import(process.argv[1]);let input='';process.stdin.on('data',c=>input+=c);process.stdin.on('end',()=>console.log(JSON.stringify(m.{export}(JSON.parse(input)))));"
    result = subprocess.run(["node", "--input-type=module", "-e", program, module.resolve().as_uri()], input=json.dumps(row), text=True, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    return json.loads(result.stdout)


def validate(validator: Path, row: dict[str, object]) -> dict[str, object]:
    return _node_module(validator, "validateOutput", row)


def score(scorer: Path, row: dict[str, object]) -> dict[str, object]:
    """Classify one row against its gold target (exact match, harness truncation, true defect)."""
    return _node_module(scorer, "scoreRow", row)


def load_tokenizer(args: argparse.Namespace):
    from transformers import AutoTokenizer
    return AutoTokenizer.from_pretrained(args.model_source, revision=args.model_revision, local_files_only=True, trust_remote_code=True)


def prompt(args: argparse.Namespace, tokenizer, text: str) -> str:
    if args.family == "sotto":
        return f"### Input\n{text}\n### Output\n"
    return tokenizer.apply_chat_template([{"role": "user", "content": text}], tokenize=False, add_generation_prompt=True)


def generation_budget(tokenizer, text: str) -> int:
    """Token-based output headroom, measured with the model's own tokenizer.

    The 2026-07-23 paid run derived this cap from `len(text.split()) * 1.5` — a
    WORD count enforced as a TOKEN limit. Protected spans are token-dense
    (`team16@birch.test` is one word but roughly seven tokens), so any output
    ending in an identifier, email, URL, or timestamp was truncated mid-span and
    then scored as a model failure. Never derive this budget from word counts.
    See docs/L1_TRAINING_RUN_2026-07-23.md, root cause 1.
    """
    return max(256, len(tokenizer.encode(text)) * 2 + 64)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True); parser.add_argument("--family", required=True)
    parser.add_argument("--model-source", required=True); parser.add_argument("--model-revision", required=True)
    parser.add_argument("artifact"); parser.add_argument("dev_set"); parser.add_argument("output")
    args = parser.parse_args()
    config = json.loads(Path(args.config).read_text("utf-8"))
    cli, validator = Path(config["llama_cli"]), Path(config["validator"])
    if not cli.is_file() or sha(cli) != config.get("llama_cli_sha256"):
        raise RuntimeError("llama-cli is missing or hash-mismatched")
    # llama.cpp b9775+ made `llama-cli` conversation-only: it rejects -no-cnv and
    # blocks on stdin waiting for a turn, so a one-shot `-p` evaluation hangs
    # forever. One-shot completion is `llama-completion`. Fail loudly rather
    # than silently hanging a scored run.
    if cli.stem == "llama-cli":
        raise RuntimeError("llama-cli is conversation-only; point config.llama_cli at llama-completion")
    if not validator.is_file() or sha(validator) != config.get("validator_sha256"):
        raise RuntimeError("output validator is missing or hash-mismatched")
    scorer = validator.with_name("score_evaluation.mjs")
    if not scorer.is_file():
        raise RuntimeError("score_evaluation.mjs is missing beside the validator")
    tokenizer = load_tokenizer(args)
    rows = [json.loads(line) for line in Path(args.dev_set).read_text("utf-8").splitlines() if line.strip()]
    generated = []
    for row in rows:
        identity = canonical_row_id(row)
        text = row["input_text"]
        budget = generation_budget(tokenizer, text)
        # stdin is closed explicitly: an interactive-mode regression must surface
        # as an error, never as an indefinite hang inside a scored run.
        result = subprocess.run([str(cli), "-m", str(Path(args.artifact).resolve()), "-p", prompt(args, tokenizer, text), "-n", str(budget), "--temp", "0", "--no-display-prompt"], text=True, capture_output=True, check=False, stdin=subprocess.DEVNULL, timeout=config.get("row_timeout_seconds", 300))
        if result.returncode:
            raise RuntimeError(result.stderr or result.stdout)
        # llama-completion prints an interactive trailer ("> EOF by user") when
        # stdin closes. It is harness chrome, not model output, and left in place
        # it turns byte-exact generations into scored quality differences.
        output = re.sub(r"\s*>\s*EOF by user\s*$", "", result.stdout.strip()).strip()
        # A generation that consumed its entire budget stopped by length, not by
        # EOS. That is a harness artifact and must never be scored as content loss.
        output_tokens = len(tokenizer.encode(output, add_special_tokens=False))
        finish_reason = "length" if output_tokens >= budget else "stop"
        # The eval row's own `output_text` IS the gold target. Preserve it as
        # `expected_output` before overwriting with the model output, or the
        # scorer has nothing to compare against and grades every row `ungraded`.
        scored_row = {**row, "expected_output": row.get("expected_output", row.get("output_text")), "output_text": output, "finish_reason": finish_reason}
        validation = validate(validator, scored_row)
        scored = score(scorer, scored_row)
        generated.append({"id": identity, "input_sha256": "sha256:" + hashlib.sha256(text.encode()).hexdigest(), "output_text": output, "finish_reason": finish_reason, "input_tokens": len(tokenizer.encode(text)), "output_tokens": output_tokens, "max_new_tokens": budget, "validation": validation, "score": scored})
    safety = {"deletion": 0, "invention": 0, "protected_span": 0, "no_op": 0, "truncation": 0}
    for row in generated:
        for error in row["validation"]["errors"]:
            if error.startswith("deletion"): safety["deletion"] += 1
            elif error.startswith("invention"): safety["invention"] += 1
            elif error.startswith("protected"): safety["protected_span"] += 1
            elif error.startswith("loop"): safety["no_op"] += 1
            elif error.startswith("truncation"): safety["truncation"] += 1
    categories: dict[str, int] = {}
    for row in generated:
        categories[row["score"]["category"]] = categories.get(row["score"]["category"], 0) + 1
    true_failures = [row["id"] for row in generated if row["score"].get("trueFailure")]
    harness_truncations = sum(1 for row in generated if row["finish_reason"] == "length")
    payload = {"schema_version": "heardright.l1.evaluation.v2", "test_only": False, "eval_input": str(Path(args.dev_set).resolve()), "eval_input_sha256": sha(args.dev_set), "rows": generated, "aggregate": {"safety": safety, "categories": categories, "true_failures": len(true_failures), "true_failure_ids": true_failures, "length_stops": harness_truncations}}
    Path(args.output).write_text(json.dumps(payload), "utf-8")


if __name__ == "__main__":
    main()
