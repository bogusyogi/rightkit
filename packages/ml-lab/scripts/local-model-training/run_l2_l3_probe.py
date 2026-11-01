#!/usr/bin/env python3
"""Probe a local checkpoint on the L2 (prompt engineering) and L3 (summarize) lanes.

L1 is a preserve-exactly task; L2 and L3 are transformative. A model
full-parameter tuned to copy its input byte-for-byte may have lost the general
instruction-following those lanes need, so this measures it rather than assuming
either way.

Uses the frozen bakeoff prompts and each row's `checks` list — the content
elements a correct output must retain — so coverage is scored objectively
instead of by taste. Writes JSONL: one row per case.
"""
from __future__ import annotations

import argparse
import json
import re
import time
from pathlib import Path


def load_cases(manifest: Path) -> list[dict]:
    """One case per distinct (mode, input); the manifest repeats each across candidates."""
    rows = json.loads(manifest.read_text("utf-8"))
    seen: dict[tuple[str, str], dict] = {}
    for row in rows:
        seen.setdefault((row["mode"], row["input_hash"]), row)
    return list(seen.values())


def covered(text: str, checks: list[str]) -> list[str]:
    """Which required content elements survive, compared case- and space-insensitively."""
    haystack = re.sub(r"\s+", " ", text.lower())
    return [check for check in checks if re.sub(r"\s+", " ", check.lower()) in haystack]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--checkpoint", required=True)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--label", default="")
    args = parser.parse_args()

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(args.checkpoint, local_files_only=True, trust_remote_code=True)
    model = AutoModelForCausalLM.from_pretrained(args.checkpoint, local_files_only=True, trust_remote_code=True, dtype=torch.bfloat16)
    device = "cuda" if torch.cuda.is_available() else "cpu"
    model.to(device).eval()

    manifest = Path(args.manifest)
    cases, results = load_cases(manifest), []
    for index, case in enumerate(cases, 1):
        payload = json.loads((manifest.parent / case["prompt_file"]).read_text("utf-8"))
        # Reuse the frozen system+user messages verbatim: same task definition the
        # hosted candidates were scored against.
        messages = [{"role": message["role"], "content": message["content"]} for message in payload["messages"]]
        encoded = tokenizer(tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True), return_tensors="pt").to(device)
        prompt_tokens = encoded["input_ids"].shape[1]
        budget = int(case.get("max_tokens") or 256)
        started = time.perf_counter()
        with torch.no_grad():
            generated = model.generate(**encoded, do_sample=False, max_new_tokens=budget, pad_token_id=tokenizer.pad_token_id or tokenizer.eos_token_id)
        new_tokens = generated[0][prompt_tokens:]
        output = tokenizer.decode(new_tokens, skip_special_tokens=True).strip()
        checks = case.get("checks") or []
        hits = covered(output, checks)
        results.append({
            "row_id": case["row_id"], "mode": case["mode"], "label": args.label,
            "input": case["input"], "output": output,
            "checks": checks, "checks_hit": hits,
            "coverage": round(len(hits) / len(checks), 3) if checks else None,
            # An L1-tuned model may simply echo its input; that is the failure to look for.
            "echoed_input": output.strip() == case["input"].strip(),
            "empty": not output.strip(),
            "output_tokens": int(len(new_tokens)), "max_new_tokens": budget,
            "latency_ms": round((time.perf_counter() - started) * 1000, 1),
        })
        print(f"  {index}/{len(cases)} {case['mode']}", flush=True)

    Path(args.output).write_text("\n".join(json.dumps(result) for result in results), "utf-8")
    for mode in ("prompt", "summary"):
        subset = [result for result in results if result["mode"] == mode]
        if not subset:
            continue
        coverages = [result["coverage"] for result in subset if result["coverage"] is not None]
        print(json.dumps({
            "lane": "L2 prompt" if mode == "prompt" else "L3 summarize",
            "cases": len(subset),
            "mean_coverage": round(sum(coverages) / len(coverages), 3) if coverages else None,
            "echoed_input": sum(1 for result in subset if result["echoed_input"]),
            "empty": sum(1 for result in subset if result["empty"]),
        }))


if __name__ == "__main__":
    main()
