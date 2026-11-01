#!/usr/bin/env python3
"""Probe safe full-weight microbatches on the bound 24 GiB Vast GPU."""
from __future__ import annotations

import argparse
import gc
import hashlib
import json
import time
from pathlib import Path


GIB = 1024 ** 3


def sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def select_configuration(results: list[dict], *, effective_batch: int, peak_limit_gib: float,
                         max_regression: float) -> dict:
    baseline = next((row for row in results if row.get("status") == "pass"), None)
    if baseline is None:
        raise ValueError("no microbatch satisfies the full-weight resource policy")
    floor = float(baseline["tokens_per_second"]) * (1.0 - max_regression)
    passing = [row for row in results if row.get("status") == "pass"
               and float(row.get("peak_vram_gib", float("inf"))) <= peak_limit_gib
               and float(row.get("tokens_per_second", 0)) >= floor
               and effective_batch % int(row["device_batch"]) == 0]
    if not passing:
        raise ValueError("no microbatch satisfies the full-weight resource policy")
    selected = max(passing, key=lambda row: int(row["device_batch"]))
    return {
        "device_batch": int(selected["device_batch"]),
        "gradient_accumulation": effective_batch // int(selected["device_batch"]),
        "effective_batch": effective_batch,
        "baseline_tokens_per_second": float(baseline["tokens_per_second"]),
        "selected_tokens_per_second": float(selected["tokens_per_second"]),
        "selected_peak_vram_gib": float(selected["peak_vram_gib"]),
    }


def build_receipt(*, family: str, model_source: str, model_revision: str,
                  training_backend: str, optimizer: str, training_input_sha256: str,
                  recipes_sha256: str, gpu: dict, policy: dict, results: list[dict],
                  selected: dict | None = None, selection_error: str | None = None) -> dict:
    return {
        "schema_version": "heardright.l1.fullft_microbatch_autotune.v1",
        "status": "ok" if selected is not None else "no_safe_microbatch",
        "family": family,
        "model_source": model_source,
        "model_revision": model_revision,
        "training_backend": training_backend,
        "optimizer": optimizer,
        "training_input_sha256": training_input_sha256,
        "recipes_sha256": recipes_sha256,
        "gpu": gpu,
        "policy": policy,
        "results": results,
        "selected": selected,
        "selection_error": selection_error,
    }


def render_rows(path: Path, tokenizer, recipe: dict, count: int) -> list[str]:
    rows = [json.loads(line) for line in path.read_text("utf-8").splitlines() if line.strip()]
    rows.sort(key=lambda row: len(row["input_text"]) + len(row["output_text"]), reverse=True)
    rendered: list[str] = []
    for row in rows[:count]:
        if recipe["serialization"]["kind"] == "literal":
            rendered.append(f"### Input\n{row['input_text']}\n### Output\n{row['output_text']}")
        else:
            messages = [{"role": "user", "content": row["input_text"]}, {"role": "assistant", "content": row["output_text"]}]
            try:
                rendered.append(tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=False))
            except Exception:
                rendered.append(f"User: {row['input_text']}\nAssistant: {row['output_text']}")
    if len(rendered) < count:
        raise ValueError(f"training input has only {len(rendered)} rows; {count} required for autotune")
    return rendered


def probe(model, tokenizer, texts: list[str], batch: int, max_length: int, warmup: int, measured: int, optimizer_name: str) -> dict:
    import torch
    if optimizer_name == "adamw_8bit":
        import bitsandbytes as bnb
        optimizer = bnb.optim.AdamW8bit(model.parameters(), lr=1e-5)
    elif optimizer_name == "torch_adamw":
        optimizer = torch.optim.AdamW(model.parameters(), lr=1e-5)
    else:
        raise ValueError(f"unsupported optimizer: {optimizer_name}")
    total_steps = warmup + measured
    tokens, elapsed = 0, 0.0
    torch.cuda.empty_cache()
    torch.cuda.reset_peak_memory_stats()
    try:
        for step in range(total_steps):
            chosen = [texts[(step * batch + offset) % len(texts)] for offset in range(batch)]
            encoded = tokenizer(chosen, return_tensors="pt", padding=True, truncation=True, max_length=max_length)
            encoded = {key: value.to("cuda") for key, value in encoded.items()}
            started = time.perf_counter()
            output = model(**encoded, labels=encoded["input_ids"])
            output.loss.backward()
            optimizer.step()
            optimizer.zero_grad(set_to_none=True)
            torch.cuda.synchronize()
            duration = time.perf_counter() - started
            if step >= warmup:
                elapsed += duration
                tokens += int(encoded["attention_mask"].sum().item())
        peak = torch.cuda.max_memory_reserved() / GIB
        return {"device_batch": batch, "status": "pass", "peak_vram_gib": round(peak, 4),
                "tokens_per_second": round(tokens / elapsed, 4), "measured_tokens": tokens,
                "measured_seconds": round(elapsed, 6)}
    except torch.OutOfMemoryError as error:
        return {"device_batch": batch, "status": "oom", "error": type(error).__name__}
    finally:
        optimizer.zero_grad(set_to_none=True)
        del optimizer
        gc.collect()
        torch.cuda.empty_cache()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--family", required=True)
    parser.add_argument("--backend", choices=("unsloth_full", "transformers_native_full"), required=True)
    parser.add_argument("--optimizer", choices=("adamw_8bit", "torch_adamw"), required=True)
    parser.add_argument("--model-source", required=True)
    parser.add_argument("--model-revision", required=True)
    parser.add_argument("--resolved-model-source")
    parser.add_argument("--training-input", required=True, type=Path)
    parser.add_argument("--recipes", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    import torch
    if not torch.cuda.is_available():
        raise SystemExit("CUDA is required for full-weight microbatch autotune")
    properties = torch.cuda.get_device_properties(0)
    if properties.total_memory < 23 * GIB:
        raise SystemExit(f"autotune requires a 24 GiB-class GPU; found {properties.total_memory / GIB:.2f} GiB")
    recipes = json.loads(args.recipes.read_text("utf-8"))
    family_recipe = recipes["families"][args.family]
    policy = recipes["common"]["full_ft_microbatch_autotune"]
    load_source, load_revision = args.model_source, args.model_revision
    if args.resolved_model_source:
        resolved = Path(args.resolved_model_source).resolve()
        if not resolved.is_dir() or resolved.name != args.model_revision:
            raise SystemExit("resolved model source is not the exact bound revision snapshot")
        load_source, load_revision = str(resolved), None
    if args.backend == "unsloth_full":
        from unsloth import FastLanguageModel
        load_kwargs = dict(
            model_name=load_source,
            max_seq_length=recipes["common"]["max_seq_length"],
            dtype=torch.bfloat16,
            load_in_4bit=False,
            load_in_8bit=False,
            full_finetuning=True,
            use_gradient_checkpointing="unsloth",
        )
        if load_revision is not None:
            load_kwargs["revision"] = load_revision
        model, tokenizer = FastLanguageModel.from_pretrained(**load_kwargs)
        FastLanguageModel.for_training(model)
    else:
        from transformers import AutoModelForCausalLM, AutoTokenizer
        load_kwargs = {"revision": load_revision} if load_revision is not None else {}
        tokenizer = AutoTokenizer.from_pretrained(load_source, **load_kwargs)
        model = AutoModelForCausalLM.from_pretrained(load_source, torch_dtype=torch.bfloat16, **load_kwargs)
        model.to("cuda")
        if hasattr(model, "gradient_checkpointing_enable"):
            model.gradient_checkpointing_enable()
        if hasattr(model.config, "use_cache"):
            model.config.use_cache = False
        for parameter in model.parameters():
            parameter.requires_grad_(True)
    texts = render_rows(args.training_input, tokenizer, family_recipe, policy["probe_rows"])
    results = [probe(model, tokenizer, texts, int(batch), recipes["common"]["max_seq_length"],
                     policy["warmup_steps"], policy["measured_steps"], args.optimizer)
               for batch in policy["candidate_device_batches"]]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    gpu = {"name": properties.name, "total_vram_gib": round(properties.total_memory / GIB, 4)}
    try:
        selected = select_configuration(results, effective_batch=policy["effective_batch"],
                                        peak_limit_gib=policy["peak_vram_limit_gib"],
                                        max_regression=policy["maximum_throughput_regression_fraction"])
    except ValueError as error:
        receipt = build_receipt(family=args.family, model_source=args.model_source, model_revision=args.model_revision, training_backend=args.backend, optimizer=args.optimizer, training_input_sha256=sha256(args.training_input), recipes_sha256=sha256(args.recipes), gpu=gpu, policy=policy, results=results, selection_error=str(error))
        temporary = args.output.with_suffix(args.output.suffix + ".tmp")
        temporary.write_text(json.dumps(receipt, indent=2) + "\n", "utf-8")
        temporary.replace(args.output)
        raise
    receipt = build_receipt(family=args.family, model_source=args.model_source, model_revision=args.model_revision, training_backend=args.backend, optimizer=args.optimizer, training_input_sha256=sha256(args.training_input), recipes_sha256=sha256(args.recipes), gpu=gpu, policy=policy, results=results, selected=selected)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n", "utf-8")
    temporary.replace(args.output)
    print(json.dumps({"status": "ok", "output": str(args.output), "selected": selected}))


if __name__ == "__main__":
    main()
