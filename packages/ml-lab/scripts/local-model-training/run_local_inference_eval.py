#!/usr/bin/env python3
"""Re-run a local checkpoint over a frozen eval set with the corrected decode policy.

Phase C of docs/L1_TRAINING_RUN_2026-07-23.md. The 2026-07-23 paid run capped
generation at `max(8, len(input_text.split()) * 1.5)` — a word count enforced as
a token limit — which truncated half the evaluation set mid-span and was then
scored as model failure. This harness uses the model's own tokenizer, greedy
decoding, and records `finish_reason` so a length stop can never again be
mistaken for content loss.

Writes JSONL rows shaped for `rescore_evaluation.mjs`.
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path


def build_prompt(tokenizer, text: str) -> str:
    """Native chat template, matching the training serialization (train = serve)."""
    return tokenizer.apply_chat_template([{"role": "user", "content": text}], tokenize=False, add_generation_prompt=True)


def render_input(row: dict, with_context: bool) -> str:
    """Legacy mode is bare input_text (the 2026-07-23 recipe). Context mode uses
    the frozen serialize_l1 contract — the format the parity retrain will use."""
    if not with_context:
        return row["input_text"]
    from serialize_l1 import serialize_l1
    return serialize_l1(row)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--checkpoint", required=True, help="directory holding model.safetensors + tokenizer")
    parser.add_argument("--eval-input", required=True, help="normalized-evaluation.jsonl")
    parser.add_argument("--output", required=True)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--headroom-multiplier", type=int, default=2)
    parser.add_argument("--headroom-floor", type=int, default=64)
    parser.add_argument("--with-context", action="store_true", help="serialize bounded_context via serialize_l1 (parity-retrain format)")
    args = parser.parse_args()
    import sys
    sys.path.insert(0, str(Path(__file__).parent))

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    checkpoint = str(Path(args.checkpoint))
    tokenizer = AutoTokenizer.from_pretrained(checkpoint, local_files_only=True, trust_remote_code=True)
    model = AutoModelForCausalLM.from_pretrained(checkpoint, local_files_only=True, trust_remote_code=True, dtype=torch.bfloat16)
    device = "cuda" if torch.cuda.is_available() else "cpu"
    model.to(device).eval()

    rows = [json.loads(line) for line in Path(args.eval_input).read_text("utf-8").splitlines() if line.strip()]
    if args.limit:
        rows = rows[: args.limit]

    eos_ids = {identifier for identifier in [tokenizer.eos_token_id, getattr(model.generation_config, "eos_token_id", None)] if isinstance(identifier, int)}
    results, started = [], time.time()
    for index, row in enumerate(rows, 1):
        text = render_input(row, args.with_context)
        encoded = tokenizer(build_prompt(tokenizer, text), return_tensors="pt").to(device)
        prompt_tokens = encoded["input_ids"].shape[1]
        # Token-based headroom measured with the model's own tokenizer. Never
        # derive this from a word count: protected spans are token-dense.
        budget = max(args.headroom_floor, len(tokenizer.encode(text)) * args.headroom_multiplier + args.headroom_floor)
        row_started = time.perf_counter()
        with torch.no_grad():
            generated = model.generate(**encoded, do_sample=False, max_new_tokens=budget, pad_token_id=tokenizer.pad_token_id or tokenizer.eos_token_id)
        if device == "cuda":
            torch.cuda.synchronize()
        elapsed_ms = round((time.perf_counter() - row_started) * 1000, 1)
        new_tokens = generated[0][prompt_tokens:]
        output = tokenizer.decode(new_tokens, skip_special_tokens=True).strip()
        # Stopped because the budget ran out, not because the model emitted EOS.
        stopped_on_eos = len(new_tokens) > 0 and int(new_tokens[-1]) in eos_ids
        finish_reason = "stop" if stopped_on_eos or len(new_tokens) < budget else "length"
        results.append({
            "id": row.get("row_id") or row["id"],
            # Always the raw transcript: the scorer's deletion denominator and
            # leak checks must never see the serialized context block.
            "input_text": row["input_text"],
            "serialized_input": text if args.with_context else None,
            "output_text": output,
            "expected_output": row["output_text"],
            "structured_spans": row.get("structured_spans", {}),
            "bounded_context": row.get("bounded_context", {}),
            "finish_reason": finish_reason,
            "input_tokens": len(tokenizer.encode(text)),
            "output_tokens": int(len(new_tokens)),
            "max_new_tokens": budget,
            "latency_ms": elapsed_ms,
        })
        if index % 20 == 0:
            print(f"  {index}/{len(rows)} rows, {time.time() - started:.0f}s", flush=True)

    Path(args.output).write_text("\n".join(json.dumps(result) for result in results), "utf-8")
    lengths = sum(1 for result in results if result["finish_reason"] == "length")
    latencies = sorted(result["latency_ms"] for result in results)
    percentile = lambda fraction: latencies[min(len(latencies) - 1, int(len(latencies) * fraction))] if latencies else 0
    print(json.dumps({"rows": len(results), "length_stops": lengths, "seconds": round(time.time() - started, 1), "latency_ms": {"p50": percentile(0.50), "p95": percentile(0.95), "max": latencies[-1] if latencies else 0}, "output": args.output}))


if __name__ == "__main__":
    main()
