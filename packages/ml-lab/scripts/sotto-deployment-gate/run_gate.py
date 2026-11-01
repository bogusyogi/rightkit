#!/usr/bin/env python3
"""Prepare and run the private Sotto deployment gate on Apple silicon."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import subprocess
import threading
import time
from pathlib import Path


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def directory_manifest(path: Path) -> dict:
    files = []
    for item in sorted(candidate for candidate in path.rglob("*") if candidate.is_file()):
        files.append({
            "path": item.relative_to(path).as_posix(),
            "bytes": item.stat().st_size,
            "sha256": file_sha256(item),
        })
    encoded = json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
    return {
        "root": str(path.resolve()),
        "files": files,
        "total_bytes": sum(item["bytes"] for item in files),
        "manifest_sha256": hashlib.sha256(encoded).hexdigest(),
    }


def load_config(path: Path) -> dict:
    config = json.loads(path.read_text(encoding="utf-8"))
    if config.get("schema") != "heardright.sotto_deployment_gate.v1":
        raise SystemExit("Unsupported or missing deployment-gate schema")
    return config


def lfm_mixed_4_6_predicate(path: str, _module) -> dict:
    """Map MLX-LM's mixed_4_6 recipe onto LFM2.5 projection names."""
    match = re.search(r"(?:^|\.)layers\.(\d+)(?:\.|$)", path)
    layer_index = int(match.group(1)) if match else 0
    number_of_layers = 16
    use_more_bits = (
        layer_index < number_of_layers // 8
        or layer_index >= 7 * number_of_layers // 8
        or (layer_index - number_of_layers // 8) % 3 == 2
    )
    high_precision_projection = path.endswith(".self_attn.v_proj") or path.endswith(
        ".feed_forward.w2"
    )
    return {
        "group_size": 64,
        "bits": 6 if use_more_bits and high_precision_projection else 4,
        "mode": "affine",
    }


def prepare(args: argparse.Namespace) -> None:
    from huggingface_hub import snapshot_download
    from mlx_lm.convert import convert

    config = load_config(args.config)
    model_root = args.model_root.resolve()
    model_root.mkdir(parents=True, exist_ok=True)
    source = config["source_model"]
    source_path = Path(snapshot_download(
        repo_id=source["repository"], revision=source["revision"]
    ))
    source_weights = source_path / "model.safetensors"
    if file_sha256(source_weights) != source["weight_sha256"]:
        raise SystemExit("Source BF16 weight hash does not match frozen config")

    control = config["fixed_5bit_control"]
    control_path = Path(snapshot_download(
        repo_id=control["repository"], revision=control["revision"]
    ))
    if file_sha256(control_path / "model.safetensors") != control["weight_sha256"]:
        raise SystemExit("Fixed 5-bit control weight hash does not match frozen config")

    registry = {
        "schema": "heardright.sotto_deployment_gate.model_registry.v1",
        "run_id": config["run_id"],
        "config_sha256": file_sha256(args.config),
        "source_snapshot": str(source_path.resolve()),
        "candidates": {},
    }
    for candidate_id in config["candidate_order"]:
        specification = config["candidates"][candidate_id]
        if specification["kind"] == "prebuilt_snapshot":
            candidate_path = control_path
        else:
            candidate_path = model_root / candidate_id
            if not candidate_path.exists():
                quant_predicate = specification.get("quant_predicate")
                if quant_predicate == "lfm_mixed_4_6_equivalent_v1":
                    quant_predicate = lfm_mixed_4_6_predicate
                conversion = {
                    "hf_path": str(source_path),
                    "mlx_path": str(candidate_path),
                    "dtype": specification.get("dtype"),
                    "quantize": specification.get("quantize", False),
                    "q_group_size": specification.get("q_group_size"),
                    "q_bits": specification.get("q_bits"),
                    "q_mode": specification.get("q_mode", "affine"),
                    "quant_predicate": quant_predicate,
                    "trust_remote_code": False,
                }
                convert(**conversion)
        manifest = directory_manifest(candidate_path)
        registry["candidates"][candidate_id] = {
            "path": str(candidate_path.resolve()),
            "specification": specification,
            "artifact": manifest,
        }
        print(json.dumps({
            "candidate": candidate_id,
            "bytes": manifest["total_bytes"],
            "manifest_sha256": manifest["manifest_sha256"],
        }))
    atomic_json(args.registry, registry)


def memory_pressure_free_percent() -> int | None:
    completed = subprocess.run(
        ["/usr/bin/memory_pressure", "-Q"],
        check=False,
        capture_output=True,
        text=True,
    )
    match = re.search(r"System-wide memory free percentage:\s*(\d+)%", completed.stdout)
    return int(match.group(1)) if match else None


def heardright_residency() -> dict:
    completed = subprocess.run(
        ["/usr/bin/pgrep", "-f", "^/Applications/HeardRight.app/Contents/MacOS/heardright-engine$"],
        check=False,
        capture_output=True,
        text=True,
    )
    pids = [int(value) for value in completed.stdout.split() if value.isdigit()]
    open_bundle_count = 0
    for pid in pids:
        opened = subprocess.run(
            ["/usr/sbin/lsof", "-p", str(pid)],
            check=False,
            capture_output=True,
            text=True,
        ).stdout
        open_bundle_count += sum(
            1 for line in opened.splitlines() if "e5bundlecache" in line
        )
    return {
        "engine_pids": pids,
        "open_e5_bundle_files": open_bundle_count,
        "resident": bool(pids and open_bundle_count),
    }


class MemorySampler:
    def __init__(self, process) -> None:
        self.process = process
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self._sample, daemon=True)
        self.minimum_available_percent = 100.0
        self.maximum_process_rss_mb = 0.0
        self.swapout_start = 0
        self.swapout_end = 0

    def _sample(self) -> None:
        import psutil

        self.swapout_start = psutil.swap_memory().sout
        while not self.stop_event.wait(0.1):
            virtual = psutil.virtual_memory()
            available = virtual.available / virtual.total * 100
            self.minimum_available_percent = min(self.minimum_available_percent, available)
            try:
                rss = self.process.memory_info().rss / 1024 / 1024
            except psutil.Error:
                break
            self.maximum_process_rss_mb = max(self.maximum_process_rss_mb, rss)
        self.swapout_end = psutil.swap_memory().sout

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self.stop_event.set()
        self.thread.join()


def run_generation(model, tokenizer, raw: str, generation: dict, sampler, processors) -> dict:
    from mlx_lm import stream_generate

    prompt = f"{generation['prompt_prefix']}{raw}{generation['prompt_suffix']}"
    max_tokens = max(
        generation["minimum_max_new_tokens"],
        math.floor(len(raw.split()) * generation["max_new_tokens_per_input_word"]),
    )
    delimiter = generation["delimiter"]
    pieces: list[str] = []
    generated_tokens = 0
    first_token_ms = None
    first_delimiter_ms = None
    peak_memory_gb = 0.0
    prompt_tokens = None
    stop_reason = "unknown"
    started = time.perf_counter()
    responses = stream_generate(
        model,
        tokenizer,
        prompt=prompt,
        max_tokens=max_tokens,
        sampler=sampler,
        logits_processors=processors,
    )
    try:
        for response in responses:
            now_ms = (time.perf_counter() - started) * 1000
            if first_token_ms is None and response.generation_tokens:
                first_token_ms = now_ms
            generated_tokens = response.generation_tokens
            prompt_tokens = response.prompt_tokens
            peak_memory_gb = max(peak_memory_gb, response.peak_memory)
            pieces.append(response.text)
            combined = "".join(pieces)
            delimiter_index = combined.find(delimiter)
            if delimiter_index >= 0:
                first_delimiter_ms = now_ms
                pieces = [combined[:delimiter_index]]
                stop_reason = "delimiter"
                break
            if response.finish_reason == "stop":
                stop_reason = "eos"
                break
            if response.finish_reason == "length":
                stop_reason = "length"
                break
    finally:
        responses.close()
    latency_ms = (time.perf_counter() - started) * 1000
    output = "".join(pieces).strip()
    return {
        "output": output,
        "output_sha256": hashlib.sha256(output.encode()).hexdigest(),
        "output_words": len(output.split()),
        "prompt_tokens": prompt_tokens,
        "generated_tokens": generated_tokens,
        "stop_reason": stop_reason,
        "time_to_first_token_ms": round(first_token_ms, 1) if first_token_ms is not None else None,
        "time_to_first_delimiter_ms": round(first_delimiter_ms, 1) if first_delimiter_ms is not None else None,
        "latency_ms": round(latency_ms, 1),
        "metal_peak_memory_gb": round(peak_memory_gb, 4),
        "max_new_tokens": max_tokens,
    }


def run(args: argparse.Namespace) -> None:
    import psutil
    from mlx_lm import load
    from mlx_lm.sample_utils import make_logits_processors, make_sampler

    config = load_config(args.config)
    if file_sha256(args.input) != config["corpus"]["cases_file_sha256"]:
        raise SystemExit("Input corpus hash does not match frozen config")
    cases = [
        json.loads(line)
        for line in args.input.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    if len(cases) != config["corpus"]["case_count"]:
        raise SystemExit("Input corpus count does not match frozen config")
    registry = json.loads(args.registry.read_text(encoding="utf-8"))
    candidate = registry["candidates"].get(args.candidate)
    if candidate is None:
        raise SystemExit(f"Candidate not in registry: {args.candidate}")

    gate = config["memory_gate"]
    residency_before = heardright_residency()
    if gate["require_heardright_engine"] and not residency_before["engine_pids"]:
        raise SystemExit("HeardRight engine is not running")
    if gate["require_open_e5_bundles"] and not residency_before["open_e5_bundle_files"]:
        raise SystemExit("HeardRight engine has no open compiled E5 bundles")

    existing = []
    if args.results.exists():
        existing = [
            json.loads(line)
            for line in args.results.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
    completed = {record["case_id"] for record in existing}
    pending = [case for case in cases if case["id"] not in completed]
    if args.limit is not None:
        pending = pending[: args.limit]
    if not pending:
        print(json.dumps({"candidate": args.candidate, "new_rows": 0, "total_rows": len(existing)}))
        return

    model, tokenizer = load(candidate["path"])
    generation = config["generation"]
    sampler = make_sampler(temp=generation["temperature"])
    processors = make_logits_processors(
        repetition_penalty=generation["repetition_penalty"],
        repetition_context_size=generation["repetition_context_size"],
    )
    run_generation(
        model,
        tokenizer,
        generation["warmup_text"],
        generation,
        sampler,
        processors,
    )

    pressure_before = memory_pressure_free_percent()
    process = psutil.Process()
    memory = MemorySampler(process)
    memory.start()
    new_rows = 0
    args.results.parent.mkdir(parents=True, exist_ok=True)
    try:
        with args.results.open("a", encoding="utf-8") as handle:
            for case in pending:
                result = run_generation(
                    model,
                    tokenizer,
                    case["text"],
                    generation,
                    sampler,
                    processors,
                )
                record = {
                    "schema": "heardright.sotto_deployment_gate.raw_result.v1",
                    "run_id": config["run_id"],
                    "candidate": args.candidate,
                    "artifact_manifest_sha256": candidate["artifact"]["manifest_sha256"],
                    "case_id": case["id"],
                    "input": case["text"],
                    "input_sha256": hashlib.sha256(case["text"].encode()).hexdigest(),
                    **result,
                    "process_rss_mb": round(process.memory_info().rss / 1024 / 1024, 1),
                }
                handle.write(json.dumps(record, ensure_ascii=False) + "\n")
                handle.flush()
                os.fsync(handle.fileno())
                new_rows += 1
                print(json.dumps({
                    "candidate": args.candidate,
                    "case_id": case["id"],
                    "generated_tokens": result["generated_tokens"],
                    "stop_reason": result["stop_reason"],
                    "latency_ms": result["latency_ms"],
                }))
                current_pressure = memory_pressure_free_percent()
                if current_pressure is not None and current_pressure < 15:
                    raise SystemExit(f"Memory circuit breaker: {current_pressure}% free")
    finally:
        memory.stop()

    pressure_after = memory_pressure_free_percent()
    residency_after = heardright_residency()
    summary = {
        "schema": "heardright.sotto_deployment_gate.lane_summary.v1",
        "run_id": config["run_id"],
        "candidate": args.candidate,
        "new_rows": new_rows,
        "total_rows": len(existing) + new_rows,
        "memory_pressure_free_before_percent": pressure_before,
        "memory_pressure_free_after_percent": pressure_after,
        "minimum_psutil_available_percent": round(memory.minimum_available_percent, 1),
        "maximum_process_rss_mb": round(memory.maximum_process_rss_mb, 1),
        "swapout_delta_bytes": memory.swapout_end - memory.swapout_start,
        "heardright_before": residency_before,
        "heardright_after": residency_after,
    }
    summary["memory_green"] = bool(
        pressure_before is not None
        and pressure_after is not None
        and pressure_before >= gate["minimum_memory_pressure_free_percent"]
        and pressure_after >= gate["minimum_memory_pressure_free_percent"]
        and summary["swapout_delta_bytes"] <= gate["maximum_swapout_delta_bytes"]
        and residency_after["resident"]
    )
    atomic_json(args.summary, summary)
    print(json.dumps(summary))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("--config", type=Path, required=True)
    prepare_parser.add_argument("--model-root", type=Path, required=True)
    prepare_parser.add_argument("--registry", type=Path, required=True)
    prepare_parser.set_defaults(function=prepare)

    run_parser = commands.add_parser("run")
    run_parser.add_argument("--config", type=Path, required=True)
    run_parser.add_argument("--registry", type=Path, required=True)
    run_parser.add_argument("--candidate", required=True)
    run_parser.add_argument("--input", type=Path, required=True)
    run_parser.add_argument("--results", type=Path, required=True)
    run_parser.add_argument("--summary", type=Path, required=True)
    run_parser.add_argument("--limit", type=int)
    run_parser.set_defaults(function=run)
    return root


if __name__ == "__main__":
    arguments = parser().parse_args()
    arguments.function(arguments)
