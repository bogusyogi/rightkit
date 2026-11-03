#!/usr/bin/env python3
"""Select a real full-weight backend after a CUDA forward/backward smoke."""
from __future__ import annotations

import argparse
import gc
import json
from pathlib import Path
from typing import Any, Callable

from train_transformers_native_full import CompletionCollator, encode_completion_example, load_rows, sha_file


def choose_first_passing(priority: list[str], probe: Callable[[str], dict[str, Any]]) -> dict[str, Any]:
    attempts = []
    for backend in priority:
        try:
            result = probe(backend)
            attempts.append({"backend": backend, "status": "pass", **result})
            return {"status": "selected", "selected": attempts[-1], "attempts": attempts}
        except Exception as error:
            attempts.append({"backend": backend, "status": "failed", "error_type": type(error).__name__, "reason": str(error)[:2000]})
    return {"status": "unsupported_for_full_weight_training", "selected": None, "attempts": attempts}


def real_probe(backend: str, *, source: str, revision: str | None, recipe: dict[str, Any], common: dict[str, Any], row: dict[str, Any]) -> dict[str, Any]:
    import torch
    import transformers
    model = tokenizer = None
    try:
        if backend == "unsloth_full":
            from unsloth import FastLanguageModel
            load_kwargs = dict(model_name=source, max_seq_length=int(common["max_seq_length"]), dtype=torch.bfloat16, load_in_4bit=False, load_in_8bit=False, full_finetuning=True, use_gradient_checkpointing="unsloth")
            if revision is not None:
                load_kwargs["revision"] = revision
            model, tokenizer = FastLanguageModel.from_pretrained(**load_kwargs)
            FastLanguageModel.for_training(model)
        elif backend == "transformers_native_full":
            from transformers import AutoModelForCausalLM, AutoTokenizer
            load_kwargs = {"revision": revision} if revision is not None else {}
            tokenizer = AutoTokenizer.from_pretrained(source, **load_kwargs)
            model = AutoModelForCausalLM.from_pretrained(source, torch_dtype=torch.bfloat16, **load_kwargs)
            model.to("cuda")
            if hasattr(model, "gradient_checkpointing_enable"):
                model.gradient_checkpointing_enable()
            if hasattr(model.config, "use_cache"):
                model.config.use_cache = False
        else:
            raise ValueError(f"unsupported full-weight backend: {backend}")
        if tokenizer.pad_token_id is None:
            if tokenizer.eos_token_id is None:
                raise RuntimeError("tokenizer has neither pad nor EOS token")
            tokenizer.pad_token = tokenizer.eos_token
        for parameter in model.parameters():
            parameter.requires_grad_(True)
        total = sum(parameter.numel() for parameter in model.parameters())
        trainable = sum(parameter.numel() for parameter in model.parameters() if parameter.requires_grad)
        if total <= 0 or trainable != total:
            raise RuntimeError(f"backend exposed {trainable}/{total} trainable parameters instead of 100%")
        encoded = encode_completion_example(tokenizer, recipe, row, int(common["max_seq_length"]))
        batch = {key: value.to("cuda") for key, value in CompletionCollator(tokenizer.pad_token_id)([encoded]).items()}
        model.train(); output = model(**batch)
        if not torch.isfinite(output.loss):
            raise RuntimeError("backend preflight produced nonfinite loss")
        output.loss.backward()
        gradient_parameters = sum(parameter.numel() for parameter in model.parameters() if parameter.grad is not None)
        if gradient_parameters <= 0:
            raise RuntimeError("backend preflight backward produced no parameter gradients")
        optimizer = "adamw_8bit"
        try:
            import bitsandbytes as bnb
            candidate = bnb.optim.AdamW8bit(model.parameters(), lr=float(recipe["training"]["full_parameter_learning_rate"]))
            candidate.step(); candidate.zero_grad(set_to_none=True); del candidate
        except Exception as error:
            if backend == "unsloth_full":
                raise RuntimeError(f"unsloth_full requires compatible adamw_8bit: {error}") from error
            for parameter in model.parameters():
                parameter.grad = None
            output = model(**batch); output.loss.backward()
            candidate = torch.optim.AdamW(model.parameters(), lr=float(recipe["training"]["full_parameter_learning_rate"]))
            candidate.step(); candidate.zero_grad(set_to_none=True); del candidate
            optimizer = "torch_adamw"
        return {"optimizer": optimizer, "model_class": type(model).__name__, "transformers_version": transformers.__version__, "total_parameters": total, "trainable_parameters": trainable, "trainable_fraction": 1.0, "gradient_parameters": gradient_parameters, "forward_backward": True, "loss": float(output.loss.detach().cpu())}
    finally:
        if model is not None:
            del model
        if tokenizer is not None:
            del tokenizer
        gc.collect()
        if torch.cuda.is_available():
            torch.cuda.empty_cache()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--family", required=True)
    parser.add_argument("--model-source", required=True)
    parser.add_argument("--model-revision", required=True)
    parser.add_argument("--resolved-model-source")
    parser.add_argument("--training-input", required=True, type=Path)
    parser.add_argument("--recipes", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    import torch
    if not torch.cuda.is_available():
        raise SystemExit("CUDA is required for full-weight backend preflight")
    recipes = json.loads(args.recipes.read_text("utf-8")); recipe, common = recipes["families"][args.family], recipes["common"]
    priority = recipe.get("training_backend", {}).get("priority")
    if not isinstance(priority, list) or not priority or any(item not in {"unsloth_full", "transformers_native_full"} for item in priority):
        raise RuntimeError(f"{args.family} has no valid full-weight backend priority")
    row = load_rows(args.training_input)[0]
    load_source, load_revision = args.model_source, args.model_revision
    if args.resolved_model_source:
        resolved = Path(args.resolved_model_source).resolve()
        if not resolved.is_dir() or resolved.name != args.model_revision:
            raise RuntimeError("resolved model source is not the exact bound revision snapshot")
        load_source, load_revision = str(resolved), None
    result = choose_first_passing(priority, lambda backend: real_probe(backend, source=load_source, revision=load_revision, recipe=recipe, common=common, row=row))
    receipt = {"schema_version": "heardright.l1.full_weight_backend_preflight.v1", "family": args.family, "model_source": args.model_source, "model_revision": args.model_revision, "resolved_model_source": load_source if args.resolved_model_source else None, "training_input": str(args.training_input.resolve()), "training_input_sha256": sha_file(args.training_input), "recipes_sha256": sha_file(args.recipes), "priority": priority, **result}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n", "utf-8"); temporary.replace(args.output)
    print(json.dumps({"status": receipt["status"], "output": str(args.output), "selected": receipt["selected"]}))


if __name__ == "__main__":
    main()
