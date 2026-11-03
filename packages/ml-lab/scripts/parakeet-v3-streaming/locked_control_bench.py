#!/usr/bin/env python3
"""Run the locked Windows TDT package through HeardRight's native runtime."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
from pathlib import Path
from typing import Any

from cache_aware_tdt_bench import (
    DurableLedger,
    sha256_file,
    validate_candidate_manifest,
)


MODEL_COMPONENTS = (
    "encoder-model.onnx",
    "decoder_joint-model.onnx",
    "mel_nemo.onnx",
    "mel_nemo.onnx.data",
    "vocab.txt",
    "config.json",
)


def runtime_model_dir(models_root: Path, checkpoint: str) -> Path:
    return models_root / checkpoint


def _digest_json(value: object) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def control_environment(
    base: dict[str, str], encoder_provider: str, decoder_provider: str
) -> dict[str, str]:
    if encoder_provider == "cpu" and decoder_provider == "dml":
        raise ValueError("product runtime cannot use DML decoder with a CPU encoder")
    environment = dict(base)
    environment["RIGHTKIT_ML_ASR_EP"] = encoder_provider
    environment["RIGHTKIT_ML_TDT_DECODER_EP"] = decoder_provider
    return environment


def build_control_command(
    control_binary: Path,
    manifest: Path,
    models_root: Path,
    out_dir: Path,
    *,
    scheduled_static: bool,
    l0_polish: bool,
) -> list[str]:
    command = [
        str(control_binary),
        "--engine",
        "tdt",
        "--manifest",
        str(manifest),
        "--models-root",
        str(models_root),
        "--out-dir",
        str(out_dir),
    ]
    if scheduled_static:
        command.append("--scheduled-static")
    if l0_polish:
        command.append("--l0-polish")
    return command


def _write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8", newline="\n") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def build_row_binding(
    item: dict[str, object], bindings: dict[str, object]
) -> dict[str, object]:
    row = {
        **bindings,
        "clip_id": str(item["id"]),
        "audio_sha256": str(item["sha256"]),
    }
    return {**row, "resume_key": _digest_json(row)}


def bind_control_rows(
    control_rows: list[dict[str, object]],
    manifest_by_id: dict[str, dict[str, object]],
    bindings: dict[str, object],
) -> list[dict[str, object]]:
    bound: list[dict[str, object]] = []
    seen: set[str] = set()
    for control in control_rows:
        clip_id = str(control.get("id", ""))
        if clip_id in seen:
            raise ValueError(f"duplicate control id: {clip_id}")
        seen.add(clip_id)
        item = manifest_by_id.get(clip_id)
        if item is None:
            raise ValueError(f"unknown control id: {clip_id}")
        decode_s = control.get("decode_s")
        decode_ms = (
            float(decode_s) * 1000.0
            if isinstance(decode_s, (int, float))
            else None
        )
        status = str(control.get("status", "error"))
        row_binding = build_row_binding(item, bindings)
        stop_tail_ms = control.get("simulated_stop_tail_ms", decode_ms)
        bound.append(
            {
                **row_binding,
                "id": clip_id,
                "status": status,
                "duration_s": float(item["duration_s"]),
                "hypothesis": str(control.get("hypothesis", "")),
                "raw_hypothesis": str(control.get("raw_hypothesis", control.get("hypothesis", ""))),
                "l0_polish_ms": control.get("l0_polish_ms"),
                "control_intent": control.get("control_intent"),
                "ai_transform": control.get("ai_transform"),
                "decode_s": decode_s,
                "decode_ms": decode_ms,
                "simulated_stop_tail_ms": stop_tail_ms,
                "total_compute_ms": control.get("total_compute_ms", decode_ms),
                "scheduled_window_decode_ms": control.get("scheduled_window_decode_ms"),
                "scheduled_background_windows": control.get("scheduled_background_windows"),
                "tail_audio_s": control.get("tail_audio_s"),
                "product_path": "AsrRuntime::transcribe_result",
                "error": None if status in {"ok", "empty"} else status,
            }
        )
    return bound


def _read_jsonl(path: Path) -> list[dict[str, object]]:
    if not path.is_file():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _harvest(
    staging_root: Path,
    manifest_by_id: dict[str, dict[str, object]],
    bindings: dict[str, object],
    ledger: DurableLedger,
) -> int:
    added = 0
    if not staging_root.is_dir():
        return added
    for attempt in sorted(staging_root.glob("attempt-*")):
        for row in bind_control_rows(
            _read_jsonl(attempt / "hypotheses.jsonl"), manifest_by_id, bindings
        ):
            if ledger.append(row):
                added += 1
    return added


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--control-binary", required=True, type=Path)
    parser.add_argument("--control-source", required=True, type=Path)
    parser.add_argument("--models-root", required=True, type=Path)
    parser.add_argument("--checkpoint", default="parakeet_tdt_v3_b64_sym_bits4_protect")
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--packet", required=True, type=Path)
    parser.add_argument("--definition", required=True, type=Path)
    parser.add_argument("--environment-lock", required=True, type=Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--candidate-id", default="windows-locked-padded")
    parser.add_argument("--encoder-provider", choices=("dml", "cpu"), default="dml")
    parser.add_argument("--decoder-provider", choices=("dml", "cpu"), default="dml")
    parser.add_argument(
        "--stop-readiness",
        choices=("full_recording_available", "scheduled_static"),
        default="full_recording_available",
    )
    parser.add_argument("--clip", action="append")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--l0-polish", action="store_true")
    args = parser.parse_args()

    rows: list[dict[str, object]] = json.loads(args.manifest.read_text(encoding="utf-8"))
    validate_candidate_manifest(rows)
    if args.clip:
        wanted = set(args.clip)
        rows = [row for row in rows if str(row["id"]) in wanted]
        missing = wanted - {str(row["id"]) for row in rows}
        if missing:
            raise ValueError(f"unknown clip id(s): {sorted(missing)}")
    if args.limit is not None:
        rows = rows[: args.limit]
    if not rows:
        raise ValueError("no clips selected")

    model_dir = runtime_model_dir(args.models_root, args.checkpoint)
    model_hashes = {
        name: sha256_file(model_dir / name) for name in MODEL_COMPONENTS
    }
    wrapper_sha = sha256_file(Path(__file__).resolve())
    control_binary_sha = sha256_file(args.control_binary)
    control_source_sha = sha256_file(args.control_source)
    runner_sha = _digest_json(
        {
            "wrapper_sha256": wrapper_sha,
            "control_binary_sha256": control_binary_sha,
            "control_source_sha256": control_source_sha,
        }
    )
    bindings: dict[str, object] = {
        "run_id": args.run_id,
        "candidate_id": args.candidate_id,
        "packet_sha256": sha256_file(args.packet),
        "definition_sha256": sha256_file(args.definition),
        "runner_sha256": runner_sha,
        "wrapper_sha256": wrapper_sha,
        "control_binary_sha256": control_binary_sha,
        "control_source_sha256": control_source_sha,
        "environment_lock_sha256": sha256_file(args.environment_lock),
        "candidate_config_sha256": _digest_json(
            {
                "engine": "tdt",
                "encoder_provider": args.encoder_provider,
                "decoder_provider": args.decoder_provider,
                "entry": "AsrRuntime::transcribe_result",
                "stop_readiness": args.stop_readiness,
                "runtime_conditioning_passes": 0,
                "l0_polish": args.l0_polish,
            }
        ),
        "model_component_sha256s": model_hashes,
    }

    manifest_by_id = {str(row["id"]): row for row in rows}
    for row in rows:
        audio_path = Path(str(row["path"]))
        if sha256_file(audio_path) != row["sha256"]:
            raise ValueError(f"audio hash mismatch: {row['id']}")

    config_key = _digest_json(bindings)
    staging_root = args.out.parent / f"{args.out.stem}.staging" / config_key
    ledger = DurableLedger(args.out)
    harvested = _harvest(staging_root, manifest_by_id, bindings, ledger)
    if harvested:
        print(f"harvested={harvested}", flush=True)

    pending = [
        row
        for row in rows
        if build_row_binding(row, bindings)["resume_key"] not in ledger.rows
    ]
    if not pending:
        print("resume_complete: no model load required", flush=True)
        return 0

    staging_root.mkdir(parents=True, exist_ok=True)
    attempt_index = len(list(staging_root.glob("attempt-*"))) + 1
    attempt = staging_root / f"attempt-{attempt_index:03d}"
    attempt.mkdir(parents=True, exist_ok=False)
    staged_manifest = attempt / "candidate-manifest.json"
    _write_json_atomic(staged_manifest, pending)
    _write_json_atomic(
        attempt / "request.json",
        {
            "config_key": config_key,
            "bindings": bindings,
            "row_ids": [row["id"] for row in pending],
        },
    )

    command = build_control_command(
        args.control_binary,
        staged_manifest,
        model_dir,
        attempt,
        scheduled_static=args.stop_readiness == "scheduled_static",
        l0_polish=args.l0_polish,
    )
    creationflags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    completed = subprocess.run(
        command,
        check=False,
        creationflags=creationflags,
        env=control_environment(
            dict(os.environ), args.encoder_provider, args.decoder_provider
        ),
    )
    _harvest(staging_root, manifest_by_id, bindings, ledger)
    if completed.returncode != 0:
        return completed.returncode

    remaining = [
        row
        for row in pending
        if build_row_binding(row, bindings)["resume_key"] not in ledger.rows
    ]
    if remaining:
        raise RuntimeError(f"control returned without {len(remaining)} row(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
