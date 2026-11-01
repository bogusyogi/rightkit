"""Crash-isolated positive-stage driver for Windows Realtime EOU bakeoff."""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
from typing import Callable


POSITIVE_IDS = tuple(f"take_{i}" for i in (1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15))
PROVIDERS = ("dml",)
SCORES = (0.0, 5.0)


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


def positive_row_specs(candidates: list[dict[str, object]]) -> list[dict[str, object]]:
    return [
        {"candidate": str(candidate["name"]), "provider": provider, "score": score}
        for candidate in candidates
        for provider in PROVIDERS
        for score in SCORES
    ]


def score_key(score: float) -> str:
    return str(score).replace(".", "_")


def checkpoint_dir(out: Path, spec: dict[str, object]) -> Path:
    return out / "checkpoints" / str(spec["candidate"]) / str(spec["provider"]) / score_key(float(spec["score"]))


def summarize_row(out: Path, spec: dict[str, object], runner_hash: str) -> dict[str, object]:
    valid: list[dict[str, object]] = []
    directory = checkpoint_dir(out, spec)
    for take in POSITIVE_IDS:
        path = directory / f"{take}.json"
        if not path.is_file():
            continue
        try:
            row = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        audio = row.get("audio") or {}
        if row.get("runner_sha256") == runner_hash and audio.get("id") == take and audio.get("negative") is False:
            valid.append(row)
    passes = sum(row.get("eou_t_s") is not None and row.get("parser_result") is not None for row in valid)
    return {
        **spec,
        "status": "complete" if len(valid) == len(POSITIVE_IDS) else "pending",
        "checkpoints": len(valid),
        "passes": passes,
    }


def drive_rows(
    specs: list[dict[str, object]],
    run_row: Callable[[dict[str, object]], dict[str, object]],
    status_path: Path,
) -> list[dict[str, object]]:
    outcomes: list[dict[str, object]] = []
    for spec in specs:
        outcomes.append({**spec, **run_row(spec)})
        counts = dict(Counter(str(row["status"]) for row in outcomes))
        atomic_json(
            status_path,
            {
                "schema": "heardright.windows-eou-positive-stage-status.v1",
                "updated_at": datetime.now(timezone.utc).isoformat(),
                "counts": counts,
                "processed": len(outcomes),
                "total": len(specs),
                "rows": outcomes,
            },
        )
    return outcomes


def crash_receipt_path(out: Path, spec: dict[str, object]) -> Path:
    return out / "row-status" / str(spec["candidate"]) / str(spec["provider"]) / f"{score_key(float(spec['score']))}.json"


def interpreter_for(args: argparse.Namespace, spec: dict[str, object]) -> Path:
    if spec["provider"] != "dml":
        raise RuntimeError("qualification driver is DML only")
    return args.dml_python


def provider_provenance(interpreter: Path, required_provider: str) -> dict[str, object]:
    probe = "import json,sys,onnxruntime as ort; print(json.dumps({'interpreter':sys.executable,'onnxruntime':ort.__version__,'providers':ort.get_available_providers()}))"
    completed = subprocess.run([str(interpreter), "-c", probe], capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        raise RuntimeError(f"provider preflight failed: {interpreter}: {completed.stderr.strip()}")
    payload = json.loads(completed.stdout)
    if required_provider not in payload["providers"]:
        raise RuntimeError(f"required provider unavailable: {required_provider} via {payload['interpreter']}: {payload['providers']}")
    return payload


def row_command(args: argparse.Namespace, spec: dict[str, object]) -> list[str]:
    return [
        str(interpreter_for(args, spec)),
        str(args.runner),
        "--receipt", str(args.receipt),
        "--audio-root", str(args.audio_root),
        "--interference", str(args.interference),
        "--mel", str(args.mel),
        "--canonical-eval", str(args.canonical_eval),
        "--parser-helper", str(args.parser_helper),
        "--out", str(args.out),
        "--mode", "qualification",
        "--candidate", str(spec["candidate"]),
        "--provider", str(spec["provider"]),
        "--score", str(spec["score"]),
    ]


def execute_row(args: argparse.Namespace, spec: dict[str, object], runner_hash: str, parent_provenance: dict[str, object]) -> dict[str, object]:
    summary = summarize_row(args.out, spec, runner_hash)
    receipt_path = crash_receipt_path(args.out, spec)
    if summary["status"] == "complete":
        outcome = {**summary, "runner_sha256": runner_hash, "runtime_provenance": parent_provenance, "reused": True}
        atomic_json(receipt_path, outcome)
        return outcome
    if receipt_path.is_file():
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        if receipt.get("runner_sha256") == runner_hash and receipt.get("runtime_provenance") == parent_provenance:
            return {**receipt, "reused": True}

    child_provenance = provider_provenance(interpreter_for(args, spec), "DmlExecutionProvider")
    if child_provenance != parent_provenance:
        raise RuntimeError(f"provider provenance changed before row: {spec}")
    command = row_command(args, spec)
    log_dir = args.out / "row-logs" / str(spec["candidate"])
    log_dir.mkdir(parents=True, exist_ok=True)
    stem = f"{spec['provider']}-{score_key(float(spec['score']))}"
    with (log_dir / f"{stem}.out.log").open("a", encoding="utf-8") as stdout, (
        log_dir / f"{stem}.err.log"
    ).open("a", encoding="utf-8") as stderr:
        completed = subprocess.run(command, stdout=stdout, stderr=stderr, check=False)

    summary = summarize_row(args.out, spec, runner_hash)
    if completed.returncode == 0 and summary["status"] == "complete":
        outcome = {**summary, "runner_sha256": runner_hash, "runtime_provenance": child_provenance, "returncode": 0, "reused": False}
        atomic_json(receipt_path, outcome)
        return outcome
    outcome = {
        **spec,
        "status": "runtime_crash" if completed.returncode != 0 else "incomplete",
        "returncode": completed.returncode,
        "runner_sha256": runner_hash,
        "runtime_provenance": child_provenance,
        "checkpoints": summary["checkpoints"],
        "passes": summary["passes"],
        "command": command,
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "reused": False,
    }
    atomic_json(receipt_path, outcome)
    return outcome


def parse_args() -> argparse.Namespace:
    scripts = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser()
    parser.add_argument("--runner", type=Path, default=scripts / "windows_realtime_eou_bakeoff.py")
    parser.add_argument("--dml-python", type=Path, default=scripts.parent.parent / ".venv-dml-eval" / "Scripts" / "python.exe")
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--audio-root", type=Path, required=True)
    parser.add_argument("--interference", type=Path, required=True)
    parser.add_argument("--mel", type=Path, required=True)
    parser.add_argument("--canonical-eval", type=Path, default=scripts / "open-asr-bakeoff" / "canonical-eval")
    parser.add_argument(
        "--parser-helper",
        type=Path,
        default=scripts.parent / "heardright-engine" / "target" / "debug" / "examples" / "eou_control_parser.exe",
    )
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--candidate", action="append")
    parser.add_argument("--provider", choices=PROVIDERS, action="append")
    parser.add_argument("--score", type=float, choices=SCORES, action="append")
    parser.add_argument("--limit-rows", type=int)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
    candidates = receipt["candidates"]
    if len(candidates) != 20 or len({row["name"] for row in candidates}) != 20:
        raise RuntimeError("receipt must contain exactly 20 unique candidates")
    specs = positive_row_specs(candidates)
    if args.candidate:
        specs = [row for row in specs if row["candidate"] in set(args.candidate)]
    if args.provider:
        specs = [row for row in specs if row["provider"] in set(args.provider)]
    if args.score:
        specs = [row for row in specs if row["score"] in set(args.score)]
    if args.limit_rows is not None:
        specs = specs[: args.limit_rows]
    runner_hash = sha256(args.runner)
    provenance = {"dml": provider_provenance(args.dml_python, "DmlExecutionProvider")}
    outcomes = drive_rows(
        specs,
        lambda spec: execute_row(args, spec, runner_hash, provenance[str(spec["provider"])]),
        args.out / "positive-stage-status.json",
    )
    counts = Counter(str(row["status"]) for row in outcomes)
    print(f"positive stage processed={len(outcomes)}/{len(specs)} counts={dict(counts)}")
    return 0 if not counts.get("incomplete") else 1


if __name__ == "__main__":
    raise SystemExit(main())
