#!/usr/bin/env python3
"""Native Transformers full-parameter trainer for unsupported Unsloth architectures."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import shutil
from pathlib import Path
from typing import Any


TRAINER_STATE_FILE = "trainer_state.json"

def sha_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def sha_tree(directory: Path) -> str:
    entries = [[item.relative_to(directory).as_posix(), item.stat().st_size, sha_file(item)] for item in sorted(directory.rglob("*")) if item.is_file()]
    return "sha256:" + hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()


def longest_common_prefix(left: list[int], right: list[int]) -> int:
    index = 0
    while index < min(len(left), len(right)) and left[index] == right[index]:
        index += 1
    return index


def render_texts(tokenizer: Any, recipe: dict[str, Any], input_text: str, output_text: str) -> tuple[str, str]:
    serialization = recipe["serialization"]
    if serialization["kind"] == "literal":
        template = serialization["fallback"]
        return template.replace("{input}", input_text).replace("{output}", ""), template.replace("{input}", input_text).replace("{output}", output_text)
    messages = [{"role": "user", "content": input_text}]
    try:
        prompt = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
        full = tokenizer.apply_chat_template([*messages, {"role": "assistant", "content": output_text}], tokenize=False, add_generation_prompt=False)
        if not isinstance(prompt, str) or not isinstance(full, str) or not prompt or not full:
            raise ValueError("chat template returned empty or non-string text")
        return prompt, full
    except Exception:
        template = serialization["fallback"]
        return template.replace("{input}", input_text).replace("{output}", ""), template.replace("{input}", input_text).replace("{output}", output_text)


def encode_completion_example(tokenizer: Any, recipe: dict[str, Any], row: dict[str, Any], max_length: int) -> dict[str, list[int]]:
    if not isinstance(row.get("input_text"), str) or not isinstance(row.get("output_text"), str):
        raise ValueError("training rows require string input_text and output_text")
    prompt, full = render_texts(tokenizer, recipe, row["input_text"], row["output_text"])
    prompt_ids = list(tokenizer(prompt, add_special_tokens=False)["input_ids"])
    full_ids = list(tokenizer(full, add_special_tokens=False)["input_ids"])
    boundary = longest_common_prefix(prompt_ids, full_ids)
    if boundary == 0 or boundary >= len(full_ids):
        raise ValueError("family serializer did not produce a measurable assistant completion boundary")
    if len(full_ids) > max_length:
        overflow = len(full_ids) - max_length
        removable_prompt = max(0, boundary - 1)
        remove = min(overflow, removable_prompt)
        full_ids = full_ids[remove:]
        boundary -= remove
        if len(full_ids) > max_length:
            full_ids = full_ids[:max_length]
    if boundary >= len(full_ids):
        raise ValueError("max sequence length removed the entire assistant completion")
    labels = [-100] * boundary + full_ids[boundary:]
    return {"input_ids": full_ids, "attention_mask": [1] * len(full_ids), "labels": labels}


def pad_completion_batch(rows: list[dict[str, list[int]]], pad_token_id: int) -> dict[str, list[list[int]]]:
    width = max(len(row["input_ids"]) for row in rows)
    padded = {"input_ids": [], "attention_mask": [], "labels": []}
    for row in rows:
        padding = width - len(row["input_ids"])
        padded["input_ids"].append(row["input_ids"] + [int(pad_token_id)] * padding)
        padded["attention_mask"].append(row["attention_mask"] + [0] * padding)
        padded["labels"].append(row["labels"] + [-100] * padding)
    return padded


class CompletionCollator:
    def __init__(self, pad_token_id: int):
        self.pad_token_id = int(pad_token_id)

    def __call__(self, rows: list[dict[str, list[int]]]) -> dict[str, Any]:
        import torch
        padded = pad_completion_batch(rows, self.pad_token_id)
        return {key: torch.tensor(value, dtype=torch.long) for key, value in padded.items()}


def load_rows(path: Path) -> list[dict[str, Any]]:
    rows = [json.loads(line) for line in path.read_text("utf-8").splitlines() if line.strip()]
    if not rows:
        raise ValueError("training input contains no rows")
    return rows


def resolve_resume(value: str | None) -> tuple[Path | None, dict[str, Any] | None]:
    if not value:
        return None, None
    manifest_path = Path(value).resolve()
    manifest = json.loads(manifest_path.read_text("utf-8"))
    checkpoint = Path(manifest.get("checkpoint_dir", "")).resolve()
    if not checkpoint.is_dir() or manifest.get("checkpoint_dir_sha256") != sha_tree(checkpoint):
        raise RuntimeError("resume checkpoint is missing or hash-mismatched")
    if manifest.get("training_backend") != "transformers_native_full":
        raise RuntimeError("native full trainer can resume only a native full checkpoint")
    return checkpoint, manifest


def make_optimizer(model: Any, optimizer_name: str, learning_rate: float, weight_decay: float):
    if optimizer_name == "adamw_8bit":
        import bitsandbytes as bnb
        return bnb.optim.AdamW8bit(model.parameters(), lr=learning_rate, weight_decay=weight_decay)
    if optimizer_name != "torch_adamw":
        raise ValueError(f"unsupported native optimizer: {optimizer_name}")
    import torch
    return torch.optim.AdamW(model.parameters(), lr=learning_rate, weight_decay=weight_decay)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--family", required=True)
    parser.add_argument("--action", choices=("train",), required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--training-input", required=True, type=Path)
    parser.add_argument("--model-source", required=True)
    parser.add_argument("--model-revision", required=True)
    parser.add_argument("--recipes", required=True, type=Path)
    parser.add_argument("--optimizer", choices=("adamw_8bit", "torch_adamw"), required=True)
    parser.add_argument("--mode", choices=("full",), required=True)
    parser.add_argument("--epochs", required=True, type=int)
    parser.add_argument("--batch-size", required=True, type=int)
    parser.add_argument("--gradient-accumulation", required=True, type=int)
    parser.add_argument("--resume-from-checkpoint")
    parser.add_argument("--selection-artifact")
    parser.add_argument("--selection-artifact-sha256")
    parser.add_argument("--selected-template-sha256")
    parser.add_argument("--selected-module-manifest-sha256")
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    if not args.live:
        raise SystemExit("native full training requires --live; tests must exercise pure helpers or explicit fakes")

    import torch
    import transformers
    from torch.utils.data import DataLoader
    from transformers import AutoModelForCausalLM, AutoTokenizer, get_linear_schedule_with_warmup

    if not torch.cuda.is_available():
        raise SystemExit("CUDA is required for native full-weight training")
    recipes = json.loads(args.recipes.read_text("utf-8"))
    recipe, common = recipes["families"][args.family], recipes["common"]
    if "transformers_native_full" not in recipe.get("training_backend", {}).get("priority", []):
        raise RuntimeError(f"{args.family} recipe does not authorize transformers_native_full")
    random.seed(int(common["seed"])); torch.manual_seed(int(common["seed"])); torch.cuda.manual_seed_all(int(common["seed"]))
    resume_checkpoint, resume_manifest = resolve_resume(args.resume_from_checkpoint)
    model_path = str(resume_checkpoint) if resume_checkpoint else args.model_source
    tokenizer = AutoTokenizer.from_pretrained(model_path, revision=None if resume_checkpoint else args.model_revision)
    if tokenizer.pad_token_id is None:
        if tokenizer.eos_token_id is None:
            raise RuntimeError("tokenizer has neither pad nor EOS token")
        tokenizer.pad_token = tokenizer.eos_token
    model = AutoModelForCausalLM.from_pretrained(model_path, revision=None if resume_checkpoint else args.model_revision, torch_dtype=torch.bfloat16)
    model.to("cuda")
    if hasattr(model, "gradient_checkpointing_enable"):
        model.gradient_checkpointing_enable()
    if hasattr(model.config, "use_cache"):
        model.config.use_cache = False
    for parameter in model.parameters():
        parameter.requires_grad_(True)
    total_parameters = sum(parameter.numel() for parameter in model.parameters())
    trainable_parameters = sum(parameter.numel() for parameter in model.parameters() if parameter.requires_grad)
    if total_parameters <= 0 or trainable_parameters != total_parameters:
        raise RuntimeError("native backend did not expose 100% trainable parameters")

    encoded = [encode_completion_example(tokenizer, recipe, row, int(common["max_seq_length"])) for row in load_rows(args.training_input)]
    generator = torch.Generator().manual_seed(int(common["seed"]))
    loader = DataLoader(encoded, batch_size=args.batch_size, shuffle=True, generator=generator, collate_fn=CompletionCollator(tokenizer.pad_token_id))
    optimizer = make_optimizer(model, args.optimizer, float(recipe["training"]["full_parameter_learning_rate"]), float(common["weight_decay"]))
    optimizer_steps_per_epoch = math.ceil(len(loader) / args.gradient_accumulation)
    total_steps = optimizer_steps_per_epoch * int(recipe["training"]["full_parameter_epochs"])
    warmup_steps = max(1, round(total_steps * float(common["warmup_ratio"])))
    scheduler = get_linear_schedule_with_warmup(optimizer, num_warmup_steps=warmup_steps, num_training_steps=total_steps)
    completed_epochs = int((resume_manifest or {}).get("completed_epochs", 0))
    global_step = int((resume_manifest or {}).get("global_step", 0))
    if resume_checkpoint:
        optimizer.load_state_dict(torch.load(resume_checkpoint / "optimizer.pt", map_location="cuda", weights_only=True))
        scheduler.load_state_dict(torch.load(resume_checkpoint / "scheduler.pt", map_location="cpu", weights_only=True))
    if args.epochs != completed_epochs + 1:
        raise RuntimeError(f"native checkpoint progression must advance exactly one epoch ({completed_epochs} -> {completed_epochs + 1}), requested {args.epochs}")

    model.train(); optimizer.zero_grad(set_to_none=True)
    measured_loss = 0.0
    for batch_index, batch in enumerate(loader, start=1):
        batch = {key: value.to("cuda") for key, value in batch.items()}
        loss = model(**batch).loss / args.gradient_accumulation
        loss.backward(); measured_loss += float(loss.detach().cpu()) * args.gradient_accumulation
        if batch_index % args.gradient_accumulation == 0 or batch_index == len(loader):
            torch.nn.utils.clip_grad_norm_(model.parameters(), float(common["gradient_clipping"]))
            optimizer.step(); scheduler.step(); optimizer.zero_grad(set_to_none=True); global_step += 1

    output = args.output_dir.resolve(); output.mkdir(parents=True, exist_ok=True)
    checkpoint = output / "checkpoint"
    temporary = output / f"checkpoint.partial-{os.getpid()}"
    if checkpoint.exists():
        raise RuntimeError(f"native checkpoint destination already exists without a stage receipt: {checkpoint}")
    if temporary.exists():
        shutil.rmtree(temporary)
    temporary.mkdir()
    model.save_pretrained(temporary, safe_serialization=True)
    tokenizer.save_pretrained(temporary)
    torch.save(optimizer.state_dict(), temporary / "optimizer.pt")
    torch.save(scheduler.state_dict(), temporary / "scheduler.pt")
    (temporary / TRAINER_STATE_FILE).write_text(json.dumps({"completed_epochs": args.epochs, "global_step": global_step, "seed": int(common["seed"])}, indent=2) + "\n", "utf-8")
    temporary.replace(checkpoint)
    payload = {
        "schema_version": "heardright.l1.action_manifest.v1", "action": "train", "family": args.family,
        "training_backend": "transformers_native_full", "transformers_version": transformers.__version__, "optimizer": args.optimizer,
        "full_parameter_training": True, "total_parameters": total_parameters, "trainable_parameters": trainable_parameters,
        "trainable_fraction": 1.0, "token_level_completion_labels": True, "dynamic_padding": True,
        "gradient_checkpointing": hasattr(model, "gradient_checkpointing_enable"), "checkpoint_dir": str(checkpoint),
        "checkpoint_dir_sha256": sha_tree(checkpoint), "model_source": args.model_source, "model_revision": args.model_revision,
        "training_input": str(args.training_input.resolve()), "training_input_sha256": sha_file(args.training_input),
        "recipes_sha256": sha_file(args.recipes), "completed_epochs": args.epochs, "global_step": global_step,
        "mean_epoch_loss": measured_loss / max(1, len(loader)), "device_batch": args.batch_size,
        "gradient_accumulation": args.gradient_accumulation, "effective_batch": args.batch_size * args.gradient_accumulation,
        "resume_manifest": str(Path(args.resume_from_checkpoint).resolve()) if args.resume_from_checkpoint else None,
    }
    if args.selection_artifact:
        payload.update({"selection_artifact": str(Path(args.selection_artifact).resolve()), "selection_artifact_sha256": args.selection_artifact_sha256, "selected_template_sha256": args.selected_template_sha256, "selected_module_manifest_sha256": args.selected_module_manifest_sha256})
    manifest = output / "train.manifest.json"
    temporary_manifest = manifest.with_suffix(".tmp")
    temporary_manifest.write_text(json.dumps(payload, indent=2) + "\n", "utf-8"); temporary_manifest.replace(manifest)
    print(json.dumps({"status": "ok", "artifact": str(manifest), "checkpoint_dir": str(checkpoint), "checkpoint_dir_sha256": payload["checkpoint_dir_sha256"], "training_backend": payload["training_backend"]}))


if __name__ == "__main__":
    main()
