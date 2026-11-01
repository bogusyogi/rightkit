#!/usr/bin/env python3
"""Version-locked launcher for the recovered, previously tested training runner.

The source checkout was removed by a concurrent release clean after compilation.
The exact CPython 3.11 runner is retained and SHA-256 bound here so the paid smoke
can execute the already-tested implementation without silently substituting code.
"""
from __future__ import annotations

import hashlib
import json
import os
import runpy
import sys
from pathlib import Path

RUNNER = Path(__file__).with_name("recovered_train_runner.pyc")
EXPECTED_SHA256 = "a911da2dabee3c9333e11f7e8cfd9baab164816b81505722e0af22cf4dfed6dd"


def full_finetuning_loader_kwargs(kwargs: dict[str, object]) -> dict[str, object]:
    """Return the non-negotiable Unsloth full-weight load configuration.

    The recovered runner was written for a LoRA-primary packet and only omitted
    `get_peft_model` for its challenger.  That is not an auditable request for
    Unsloth full fine-tuning.  Keep this tiny pure function separately testable
    and inject it into the version-locked runner at process start.
    """
    configured = dict(kwargs)
    configured.update({
        "load_in_4bit": False,
        "load_in_8bit": False,
        "dtype": "bf16",
        "full_finetuning": True,
        "use_gradient_checkpointing": "unsloth",
    })
    return configured


def patch_unsloth_for_full_finetuning(canonical_source: str | None = None, resolved_source: str | None = None) -> None:
    """Force the recovered runner through the documented Unsloth full-FT API."""
    import torch
    from unsloth import FastLanguageModel

    original = FastLanguageModel.from_pretrained

    def from_pretrained(*args, **kwargs):
        configured = full_finetuning_loader_kwargs(kwargs)
        positional = list(args)
        if canonical_source and resolved_source:
            if configured.get("model_name") == canonical_source:
                configured["model_name"] = resolved_source
                configured.pop("revision", None)
            elif positional and positional[0] == canonical_source:
                positional[0] = resolved_source
                configured.pop("revision", None)
        configured["dtype"] = torch.bfloat16
        return original(*positional, **configured)

    FastLanguageModel.from_pretrained = staticmethod(from_pretrained)


def consume_resolved_source_args(argv: list[str]) -> tuple[list[str], str | None, str | None]:
    rewritten = list(argv)
    values = {}
    for name in ["--resolved-model-source", "--resolved-source-receipt"]:
        if name not in rewritten:
            continue
        index = rewritten.index(name)
        if index + 1 >= len(rewritten) or rewritten[index + 1].startswith("--"):
            raise ValueError(f"{name} requires a concrete value")
        values[name] = rewritten[index + 1]
        del rewritten[index:index + 2]
    if not values:
        return rewritten, None, None
    if set(values) != {"--resolved-model-source", "--resolved-source-receipt"}:
        raise ValueError("resolved model source and receipt must be supplied together")
    canonical = argument_value(rewritten, "--model-source")
    revision = argument_value(rewritten, "--model-revision")
    resolved = Path(values["--resolved-model-source"]).resolve()
    receipt_path = Path(values["--resolved-source-receipt"]).resolve()
    receipt = json.loads(receipt_path.read_text("utf-8"))
    matches = [entry for entry in receipt.get("entries", []) if entry.get("source_id") == canonical and entry.get("revision") == revision]
    if receipt.get("schema_version") != "heardright.l1.exact_source_resolution.v1" or receipt.get("status") != "passed" or len(matches) != 1 or Path(matches[0]["resolved_path"]).resolve() != resolved or not resolved.is_dir() or resolved.name != revision:
        raise ValueError("resolved model source does not match its exact source-resolution receipt")
    return rewritten, canonical, str(resolved)


def canonical_evaluation_id(row: dict[str, object]) -> str:
    row_id, legacy_id = row.get("row_id"), row.get("id")
    if row_id is not None and legacy_id is not None and row_id != legacy_id:
        raise ValueError("evaluation row has conflicting id and row_id")
    identity = row_id if row_id is not None else legacy_id
    if not isinstance(identity, str) or not identity:
        raise ValueError("evaluation row requires a non-empty string identifier (row_id or id)")
    if not isinstance(row.get("input_text"), str):
        raise ValueError("evaluation row requires string input_text")
    return identity


def file_sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def argument_value(argv: list[str], name: str) -> str:
    try:
        index = argv.index(name) + 1
    except ValueError as error:
        raise ValueError(f"{name} is required") from error
    if index >= len(argv) or argv[index].startswith("--"):
        raise ValueError(f"{name} requires a concrete value")
    return argv[index]


def atomic_json(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f"{path.name}.tmp-{os.getpid()}")
    temporary.write_text(json.dumps(payload, indent=2) + "\n", "utf-8")
    temporary.replace(path)


def normalized_evaluation_view(source: Path, output_dir: Path) -> tuple[Path, str]:
    """Create and retain the opaque runner's canonical `id` view in its stage."""
    source = source.resolve()
    source_sha256 = file_sha256(source)
    output_dir = output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    view = output_dir / "normalized-evaluation.jsonl"
    if view.resolve() == source:
        raise ValueError("normalized evaluation view must not replace its frozen source")
    rendered: list[str] = []
    for number, line in enumerate(source.read_text("utf-8").splitlines(), start=1):
        if not line.strip():
            continue
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError(f"evaluation row {number} is not an object")
        identity = canonical_evaluation_id(row)
        rendered.append(json.dumps({**row, "id": identity}, ensure_ascii=False, separators=(",", ":")))
    if not rendered:
        raise ValueError("evaluation input contains no rows")
    view.write_text("\n".join(rendered) + "\n", "utf-8")
    if file_sha256(source) != source_sha256:
        raise RuntimeError("frozen evaluation input changed while creating normalized view")
    return view, source_sha256


def finalize_evaluation_contract(source: Path, source_sha256: str, view: Path, output_dir: Path) -> None:
    source, view, output_dir = source.resolve(), view.resolve(), output_dir.resolve()
    if file_sha256(source) != source_sha256:
        raise RuntimeError("frozen evaluation input changed while evaluating normalized view")
    if not view.is_file() or view.parent != output_dir:
        raise RuntimeError("durable normalized evaluation view is missing or outside the evaluation output directory")
    view_sha256 = file_sha256(view)
    evaluation_path = output_dir / "evaluation.json"
    manifest_path = output_dir / "evaluate.manifest.json"
    if not evaluation_path.is_file() or not manifest_path.is_file():
        raise RuntimeError("opaque evaluator did not emit evaluation.json and evaluate.manifest.json")
    evaluation = json.loads(evaluation_path.read_text("utf-8"))
    manifest = json.loads(manifest_path.read_text("utf-8"))
    if not isinstance(evaluation, dict) or not isinstance(manifest, dict):
        raise RuntimeError("opaque evaluator emitted an invalid evaluation contract")
    evaluation.update({
        "eval_input": str(view),
        "eval_input_sha256": view_sha256,
        "source_eval_input": str(source),
        "source_eval_input_sha256": source_sha256,
    })
    atomic_json(evaluation_path, evaluation)
    manifest.update({
        "evaluation_artifact": str(evaluation_path),
        "evaluation_sha256": file_sha256(evaluation_path),
        "eval_input": str(view),
        "eval_input_sha256": view_sha256,
        "source_eval_input": str(source),
        "source_eval_input_sha256": source_sha256,
    })
    atomic_json(manifest_path, manifest)


def validate_export_evaluation_binding(argv: list[str]) -> None:
    validation_path = Path(argument_value(argv, "--validation-artifact")).resolve()
    eval_input = Path(argument_value(argv, "--eval-input")).resolve()
    if not validation_path.is_file() or not eval_input.is_file():
        raise ValueError("export validation artifact and durable eval input must exist")
    evaluation = json.loads(validation_path.read_text("utf-8"))
    if Path(str(evaluation.get("eval_input", ""))).resolve() != eval_input:
        raise ValueError("export eval input does not match validation artifact")
    if evaluation.get("eval_input_sha256") != file_sha256(eval_input):
        raise ValueError("export eval input hash does not match validation artifact")


def rewrite_evaluation_argv(argv: list[str], view: Path) -> list[str]:
    rewritten = list(argv)
    try:
        index = rewritten.index("--eval-input") + 1
    except ValueError as error:
        raise ValueError("evaluate action requires --eval-input") from error
    if index >= len(rewritten) or rewritten[index].startswith("--"):
        raise ValueError("evaluate action requires a concrete --eval-input path")
    rewritten[index] = str(view)
    return rewritten


def checkpoint_tree_sha(directory: Path) -> str:
    root = directory.resolve()
    entries = sorted([[item.relative_to(root).as_posix(), item.stat().st_size, file_sha256(item)] for item in root.rglob("*") if item.is_file()])
    return "sha256:" + hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode("utf-8")).hexdigest()


def rewrite_resume_checkpoint_argument(argv: list[str]) -> tuple[list[str], Path | None]:
    rewritten = list(argv)
    if "--resume-from-checkpoint" not in rewritten:
        return rewritten, None
    index = rewritten.index("--resume-from-checkpoint") + 1
    if index >= len(rewritten) or rewritten[index].startswith("--"):
        raise ValueError("--resume-from-checkpoint requires a manifest path")
    manifest_path = Path(rewritten[index]).resolve()
    manifest = json.loads(manifest_path.read_text("utf-8"))
    declared = Path(str(manifest.get("checkpoint_dir", ""))).resolve()
    candidates = [declared] if (declared / "trainer_state.json").is_file() or (declared / "trainer-state.json").is_file() else [item for item in declared.parent.glob("checkpoint-*") if item.is_dir() and ((item / "trainer_state.json").is_file() or (item / "trainer-state.json").is_file())]
    if manifest.get("action") != "train" or len(candidates) != 1:
        raise ValueError("resume manifest does not resolve exactly one populated checkpoint")
    temporary = manifest_path.with_name(f".resume-checkpoint-{os.getpid()}.json")
    atomic_json(temporary, {**manifest, "checkpoint_dir": str(candidates[0]), "checkpoint_dir_sha256": checkpoint_tree_sha(candidates[0])})
    rewritten[index] = str(temporary)
    return rewritten, temporary


def main() -> None:
    if sys.version_info[:2] != (3, 11):
        raise SystemExit(f"Recovered training runner requires CPython 3.11, got {sys.version.split()[0]}")
    if not RUNNER.is_file():
        raise SystemExit(f"Training runner is missing: {RUNNER}")
    actual = hashlib.sha256(RUNNER.read_bytes()).hexdigest()
    if actual != EXPECTED_SHA256:
        raise SystemExit(f"Training runner hash mismatch: {actual}")
    try:
        rewritten_argv, canonical_source, resolved_source = consume_resolved_source_args(sys.argv)
    except ValueError as error:
        raise SystemExit(str(error)) from error
    sys.argv = rewritten_argv
    action = sys.argv[sys.argv.index("--action") + 1] if "--action" in sys.argv else None
    resume_manifest = None
    if action == "train":
        try:
            sys.argv, resume_manifest = rewrite_resume_checkpoint_argument(sys.argv)
        except ValueError as error:
            raise SystemExit(str(error)) from error
        if "--mode" not in sys.argv or sys.argv[sys.argv.index("--mode") + 1] != "full":
            raise SystemExit("Only --mode full is authorized for four-family training")
        patch_unsloth_for_full_finetuning(canonical_source, resolved_source)
    if action in {"export", "export-q8", "validate"}:
        validate_export_evaluation_binding(sys.argv)
    if action != "evaluate":
        try:
            runpy.run_path(str(RUNNER), run_name="__main__")
        finally:
            if resume_manifest is not None:
                resume_manifest.unlink(missing_ok=True)
        return
    try:
        source = Path(argument_value(sys.argv, "--eval-input"))
        output_dir = Path(argument_value(sys.argv, "--output-dir"))
    except (ValueError, IndexError) as error:
        raise SystemExit("evaluate action requires --eval-input") from error
    original_argv = sys.argv[:]
    view, source_sha256 = normalized_evaluation_view(source, output_dir)
    try:
        sys.argv = rewrite_evaluation_argv(original_argv, view)
        runpy.run_path(str(RUNNER), run_name="__main__")
    finally:
        sys.argv = original_argv
    finalize_evaluation_contract(source, source_sha256, view, output_dir)


if __name__ == "__main__":
    main()
