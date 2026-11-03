"""Checkpoint-derived positive-stage report for Windows Realtime EOU bakeoff."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


POSITIVE_IDS = tuple(f"take_{i}" for i in (1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15))
EXPECTED_CACHE_SHAPES = [[1, 17, 70, 512], [1, 17, 512, 8]]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


def atomic_json(path: Path, payload: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def score_key(score: float) -> str:
    return str(score).replace(".", "_")


def checkpoint_dir(out: Path, spec: dict[str, object]) -> Path:
    return out / "checkpoints" / str(spec["candidate"]) / str(spec["provider"]) / score_key(float(spec["score"]))


def crash_receipt_path(out: Path, spec: dict[str, object]) -> Path:
    return out / "row-status" / str(spec["candidate"]) / str(spec["provider"]) / f"{score_key(float(spec['score']))}.json"


def positive_row_report(out: Path, spec: dict[str, object], runner_hash: str) -> dict[str, Any]:
    clips: list[dict[str, Any]] = []
    for take in POSITIVE_IDS:
        path = checkpoint_dir(out, spec) / f"{take}.json"
        if not path.is_file():
            continue
        try:
            clip = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        audio = clip.get("audio") or {}
        if clip.get("runner_sha256") == runner_hash and audio.get("id") == take and audio.get("negative") is False:
            clips.append(clip)
    complete = len(clips) == len(POSITIVE_IDS)
    controls = [clip.get("eou_t_s") is not None and clip.get("parser_result") is not None for clip in clips]
    probes = [probe for clip in clips for probe in clip.get("probes", [])]
    finite = all(clip.get("finite") is True for clip in clips)
    cache_shapes = all(clip.get("cache_shapes") == EXPECTED_CACHE_SHAPES for clip in clips)
    crash = None
    crash_path = crash_receipt_path(out, spec)
    if crash_path.is_file():
        try:
            crash = json.loads(crash_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            crash = None
    required_provider = "DmlExecutionProvider" if spec["provider"] == "dml" else "CPUExecutionProvider"
    provenance = (crash or {}).get("runtime_provenance") or {}
    provider_valid = required_provider in provenance.get("providers", [])
    runtime_valid = bool(crash and crash.get("runner_sha256") == runner_hash and crash.get("status") == "complete")
    gate_clean = complete and all(controls) and finite and cache_shapes and runtime_valid and provider_valid
    status = "runtime_crash" if crash and crash.get("runner_sha256") == runner_hash and crash.get("status") == "runtime_crash" else "complete" if complete else "pending"
    return {
        **spec,
        "status": status,
        "checkpoints": len(clips),
        "positive_passes": sum(controls),
        "finite": finite,
        "cache_shapes_valid": cache_shapes,
        "runtime_valid": runtime_valid,
        "provider_valid": provider_valid,
        "runtime_provenance": provenance,
        "logical_probe_count": len(probes),
        "gate_clean": gate_clean,
    }


def all_specs(candidates: list[dict[str, Any]]) -> list[dict[str, object]]:
    return [
        {"candidate": candidate["name"], "provider": "dml", "score": score}
        for candidate in candidates
        for score in (0.0, 5.0)
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    candidates = json.loads(args.receipt.read_text(encoding="utf-8"))["candidates"]
    if len(candidates) != 20 or len({row["name"] for row in candidates}) != 20:
        raise RuntimeError("receipt must contain exactly 20 unique candidates")
    runner_hash = sha256(args.runner)
    rows = [positive_row_report(args.out, spec, runner_hash) for spec in all_specs(candidates)]
    report = {"schema": "heardright.windows-eou-qualification-stage-report.v1", "runner_sha256": runner_hash, "rows": rows}
    atomic_json(args.out / "qualification-stage-report.json", report)
    atomic_json(
        args.out / "qualification-survivors.json",
        {
            "schema": "heardright.windows-eou-qualification-survivors.v1",
            "runner_sha256": runner_hash,
            "rows": [row for row in rows if row["gate_clean"]],
        },
    )
    print(f"qualification report rows={len(rows)} complete={sum(row['status'] == 'complete' for row in rows)} clean={sum(row['gate_clean'] for row in rows)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
