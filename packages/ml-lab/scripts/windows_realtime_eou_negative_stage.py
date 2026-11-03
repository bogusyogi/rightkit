"""Crash-isolated, shortlist-only 238-negative Windows EOU stage."""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from typing import Any


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


def row_command(args: argparse.Namespace, spec: dict[str, object], negative_ids: list[str]) -> list[str]:
    command = [
        str(args.python), str(args.runner), "--receipt", str(args.receipt), "--audio-root", str(args.audio_root),
        "--interference", str(args.interference), "--mel", str(args.mel), "--canonical-eval", str(args.canonical_eval),
        "--parser-helper", str(args.parser_helper), "--out", str(args.out), "--candidate", str(spec["candidate"]),
        "--provider", str(spec["provider"]), "--score", str(spec["score"]),
    ]
    for negative_id in negative_ids:
        command.extend(("--input", negative_id))
    return command


def checkpoint_dir(out: Path, spec: dict[str, object]) -> Path:
    return out / "checkpoints" / str(spec["candidate"]) / str(spec["provider"]) / score_key(float(spec["score"]))


def negative_summary(out: Path, spec: dict[str, object], runner_hash: str, ids: list[str]) -> dict[str, object]:
    clips: list[dict[str, Any]] = []
    for negative_id in ids:
        path = checkpoint_dir(out, spec) / f"{negative_id}.json"
        if not path.is_file():
            continue
        try:
            clip = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        audio = clip.get("audio") or {}
        if clip.get("runner_sha256") == runner_hash and audio.get("id") == negative_id and audio.get("negative") is True:
            clips.append(clip)
    zero_controls = sum(clip.get("eou_t_s") is None and clip.get("parser_result") is None for clip in clips)
    return {**spec, "status": "complete" if len(clips) == len(ids) else "pending", "checkpoints": len(clips), "zero_controls": zero_controls}


def parse_args() -> argparse.Namespace:
    scripts = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser()
    parser.add_argument("--runner", type=Path, default=scripts / "windows_realtime_eou_bakeoff.py")
    parser.add_argument("--python", type=Path, default=Path(sys.executable))
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--audio-root", type=Path, required=True)
    parser.add_argument("--interference", type=Path, required=True)
    parser.add_argument("--mel", type=Path, required=True)
    parser.add_argument("--canonical-eval", type=Path, default=scripts / "open-asr-bakeoff" / "canonical-eval")
    parser.add_argument("--parser-helper", type=Path, default=scripts.parent / "heardright-engine" / "target" / "debug" / "examples" / "eou_control_parser.exe")
    parser.add_argument("--shortlist", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest = json.loads((args.canonical_eval / "candidate_manifest.json").read_text(encoding="utf-8"))
    clips = manifest["clips"] if isinstance(manifest, dict) else manifest
    negative_ids = [str(clip["id"]) for clip in clips]
    if len(negative_ids) != 238 or len(set(negative_ids)) != 238:
        raise RuntimeError("canonical negative manifest must contain exactly 238 unique IDs")
    shortlist = json.loads(args.shortlist.read_text(encoding="utf-8"))["rows"]
    if not shortlist:
        raise RuntimeError("shortlist must contain at least one ranked row")
    runner_hash = sha256(args.runner)
    outcomes: list[dict[str, object]] = []
    for spec in shortlist:
        summary = negative_summary(args.out, spec, runner_hash, negative_ids)
        if summary["status"] != "complete":
            log_dir = args.out / "negative-row-logs" / str(spec["candidate"])
            log_dir.mkdir(parents=True, exist_ok=True)
            stem = f"{spec['provider']}-{score_key(float(spec['score']))}"
            with (log_dir / f"{stem}.out.log").open("a", encoding="utf-8") as stdout, (log_dir / f"{stem}.err.log").open("a", encoding="utf-8") as stderr:
                result = subprocess.run(row_command(args, spec, negative_ids), stdout=stdout, stderr=stderr, check=False)
            summary = {**negative_summary(args.out, spec, runner_hash, negative_ids), "returncode": result.returncode}
            if result.returncode != 0:
                summary["status"] = "runtime_crash"
        outcomes.append(summary)
        atomic_json(args.out / "negative-stage-status.json", {"schema": "heardright.windows-eou-negative-stage-status.v1", "updated_at": datetime.now(timezone.utc).isoformat(), "counts": dict(Counter(str(row["status"]) for row in outcomes)), "rows": outcomes})
    print(f"negative stage rows={len(outcomes)} pass238={sum(row['zero_controls'] == 238 for row in outcomes)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
