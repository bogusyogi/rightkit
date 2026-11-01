#!/usr/bin/env python3
"""Create Git-safe Sotto deployment-gate results from private raw outputs."""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import statistics
from pathlib import Path


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def percentile_95(values: list[float]) -> float:
    if not values:
        raise ValueError("Cannot calculate p95 of an empty lane")
    if len(values) == 1:
        return values[0]
    return statistics.quantiles(values, n=100, method="inclusive")[94]


def atomic_json(path: Path, value: object) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    path.parent.mkdir(parents=True, exist_ok=True)
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
    temporary.replace(path)


def load_jsonl(path: Path) -> list[dict]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def main(args: argparse.Namespace) -> None:
    config = json.loads(args.config.read_text(encoding="utf-8"))
    decisions = json.loads(args.decisions.read_text(encoding="utf-8"))
    registry = json.loads(args.registry.read_text(encoding="utf-8"))
    candidates = config["candidate_order"]
    expected_ids = None
    provenance_rows = []
    summary_candidates = {}
    artifact_candidates = {}

    for candidate_id in candidates:
        raw_path = args.private_root / f"full-{candidate_id}.jsonl"
        memory_path = args.private_root / f"full-{candidate_id}.summary.json"
        rows = load_jsonl(raw_path)
        if len(rows) != config["corpus"]["case_count"]:
            raise SystemExit(f"{candidate_id}: expected 30 rows, got {len(rows)}")
        row_ids = [row["case_id"] for row in rows]
        if len(set(row_ids)) != len(row_ids):
            raise SystemExit(f"{candidate_id}: duplicate case IDs")
        if expected_ids is None:
            expected_ids = row_ids
        elif row_ids != expected_ids:
            raise SystemExit(f"{candidate_id}: case order differs from BF16")
        if any(row["candidate"] != candidate_id for row in rows):
            raise SystemExit(f"{candidate_id}: candidate field mismatch")

        review = decisions["candidates"][candidate_id]
        rejected = review["rejected"]
        added = set(review["added_regression_case_ids"])
        if not set(rejected).issubset(row_ids) or not added.issubset(row_ids):
            raise SystemExit(f"{candidate_id}: review contains an unknown case ID")
        if not added.issubset(rejected):
            raise SystemExit(f"{candidate_id}: added regressions must be rejected")

        accepted = [row for row in rows if row["case_id"] not in rejected]
        all_latencies = [row["latency_ms"] for row in rows]
        accepted_latencies = [row["latency_ms"] for row in accepted]
        memory = json.loads(memory_path.read_text(encoding="utf-8"))
        stop_reasons = dict(collections.Counter(row["stop_reason"] for row in rows))
        delimiter_times = [
            row["time_to_first_delimiter_ms"]
            for row in rows
            if row["time_to_first_delimiter_ms"] is not None
        ]
        accepted_p95 = round(percentile_95(accepted_latencies), 1)
        latency_pass = accepted_p95 <= config["deployment_gate"]["accepted_output_p95_ms_max"]
        quality_pass = not added
        memory_pass = bool(memory["memory_green"])
        lane_pass = latency_pass and quality_pass and memory_pass

        summary_candidates[candidate_id] = {
            "rows": len(rows),
            "accepted_rows": len(accepted),
            "rejected_rows": len(rejected),
            "added_regression_cases": len(added),
            "added_regression_case_ids": sorted(added),
            "stop_reason_counts": stop_reasons,
            "time_to_first_delimiter_observations": len(delimiter_times),
            "time_to_first_delimiter_p95_ms": round(percentile_95(delimiter_times), 1) if delimiter_times else None,
            "generated_tokens_p50": round(statistics.median(row["generated_tokens"] for row in rows), 1),
            "generated_tokens_p95": round(percentile_95([row["generated_tokens"] for row in rows]), 1),
            "all_output_p50_ms": round(statistics.median(all_latencies), 1),
            "all_output_p95_ms": round(percentile_95(all_latencies), 1),
            "accepted_output_p95_ms": accepted_p95,
            "memory_pressure_free_before_percent": memory["memory_pressure_free_before_percent"],
            "memory_pressure_free_after_percent": memory["memory_pressure_free_after_percent"],
            "minimum_psutil_available_percent": memory["minimum_psutil_available_percent"],
            "maximum_process_rss_mb": memory["maximum_process_rss_mb"],
            "swapout_delta_bytes": memory["swapout_delta_bytes"],
            "heardright_resident_before": memory["heardright_before"]["resident"],
            "heardright_resident_after": memory["heardright_after"]["resident"],
            "latency_pass": latency_pass,
            "memory_pass": memory_pass,
            "quality_pass": quality_pass,
            "deployment_gate_pass": lane_pass,
            "raw_results_sha256": file_sha256(raw_path),
            "raw_memory_summary_sha256": file_sha256(memory_path),
        }

        artifact = registry["candidates"][candidate_id]["artifact"]
        artifact_candidates[candidate_id] = {
            "specification": registry["candidates"][candidate_id]["specification"],
            "manifest_sha256": artifact["manifest_sha256"],
            "total_bytes": artifact["total_bytes"],
            "files": [
                {
                    "path": item["path"],
                    "bytes": item["bytes"],
                    "sha256": item["sha256"],
                }
                for item in artifact["files"]
            ],
        }

        for row in rows:
            reasons = rejected.get(row["case_id"], [])
            provenance_rows.append({
                "schema": "heardright.sotto_deployment_gate.provenance.v1",
                "run_id": config["run_id"],
                "candidate": candidate_id,
                "artifact_manifest_sha256": row["artifact_manifest_sha256"],
                "case_id": row["case_id"],
                "input_sha256": row["input_sha256"],
                "output_sha256": row["output_sha256"],
                "prompt_tokens": row["prompt_tokens"],
                "generated_tokens": row["generated_tokens"],
                "stop_reason": row["stop_reason"],
                "time_to_first_token_ms": row["time_to_first_token_ms"],
                "time_to_first_delimiter_ms": row["time_to_first_delimiter_ms"],
                "latency_ms": row["latency_ms"],
                "metal_peak_memory_gb": row["metal_peak_memory_gb"],
                "process_rss_mb": row["process_rss_mb"],
                "accepted": not reasons,
                "rejection_reasons": reasons,
                "added_regression_vs_bf16": row["case_id"] in added,
            })

    summary = {
        "schema": "heardright.sotto_deployment_gate.summary.v1",
        "run_id": config["run_id"],
        "config_sha256": file_sha256(args.config),
        "runner_sha256": file_sha256(args.runner),
        "scorer_sha256": file_sha256(Path(__file__)),
        "review_decisions_sha256": file_sha256(args.decisions),
        "corpus": config["corpus"],
        "deployment_gate": config["deployment_gate"],
        "candidates": summary_candidates,
        "passing_candidates": [
            candidate_id
            for candidate_id in candidates
            if summary_candidates[candidate_id]["deployment_gate_pass"]
        ],
    }
    artifacts = {
        "schema": "heardright.sotto_deployment_gate.artifacts.v1",
        "run_id": config["run_id"],
        "registry_sha256": file_sha256(args.registry),
        "candidates": artifact_candidates,
    }
    atomic_json(args.output / "gate-summary.json", summary)
    atomic_json(args.output / "artifact-provenance.json", artifacts)
    provenance_path = args.output / "per-case-provenance.jsonl"
    with provenance_path.open("w", encoding="utf-8") as handle:
        for row in provenance_rows:
            handle.write(json.dumps(row, sort_keys=True) + "\n")
    print(json.dumps({
        "candidates": len(candidates),
        "provenance_rows": len(provenance_rows),
        "passing_candidates": summary["passing_candidates"],
    }))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--decisions", type=Path, required=True)
    parser.add_argument("--registry", type=Path, required=True)
    parser.add_argument("--private-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    main(parser.parse_args())
