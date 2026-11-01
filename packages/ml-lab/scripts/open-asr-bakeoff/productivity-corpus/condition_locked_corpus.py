from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


POLICY = "asr_simple_gain_hpf"


@dataclass(frozen=True)
class ConditionResult:
    clip_count: int
    candidate_manifest: Path
    private_manifest: Path


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _inspect_wav(path: Path) -> tuple[int, int, float]:
    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        sample_width = wav.getsampwidth()
        sample_rate = wav.getframerate()
        duration_s = wav.getnframes() / sample_rate
    if channels != 1 or sample_width != 2 or sample_rate != 16000:
        raise ValueError(
            f"conditioned audio must be mono PCM s16le at 16 kHz: {path}"
        )
    return channels, sample_rate, duration_s


def condition_locked_corpus(
    candidate_manifest: Path,
    private_truth_manifest: Path,
    output_dir: Path,
    conditioner: Callable[[Path, Path], None],
    policy_source_sha256: str,
    limit: int | None = None,
) -> ConditionResult:
    candidate_rows = json.loads(candidate_manifest.read_text(encoding="utf-8"))
    truth_rows = json.loads(private_truth_manifest.read_text(encoding="utf-8"))
    truth_by_id = {row["id"]: row for row in truth_rows}
    if len(truth_by_id) != len(truth_rows):
        raise ValueError("duplicate ids in private truth manifest")
    if limit is not None:
        candidate_rows = candidate_rows[:limit]
    if any(row["id"] not in truth_by_id for row in candidate_rows):
        raise ValueError("candidate manifest contains an id missing from private truth")

    audio_dir = output_dir / "candidate_audio"
    private_dir = output_dir / "_scorer_private"
    audio_dir.mkdir(parents=True, exist_ok=True)
    private_dir.mkdir(parents=True, exist_ok=True)

    conditioned_candidates: list[dict[str, object]] = []
    conditioned_truth: list[dict[str, object]] = []
    ledger: list[dict[str, object]] = []
    conditioned_ids: set[str] = set()
    for row in candidate_rows:
        raw_id = row["id"]
        raw_path = Path(row["path"])
        working_path = audio_dir / f"working-{raw_id}.wav"
        conditioner(raw_path, working_path)
        channels, sample_rate, duration_s = _inspect_wav(working_path)
        conditioned_sha256 = _sha256_file(working_path)
        conditioned_id = conditioned_sha256[:16]
        if conditioned_id in conditioned_ids:
            raise ValueError(f"duplicate conditioned audio id: {conditioned_id}")
        conditioned_ids.add(conditioned_id)
        conditioned_path = audio_dir / f"{conditioned_id}.wav"
        working_path.replace(conditioned_path)

        conditioned_candidates.append(
            {
                "id": conditioned_id,
                "path": str(conditioned_path.resolve()),
                "duration_s": round(duration_s, 3),
                "sha256": conditioned_sha256,
                "sample_rate_hz": sample_rate,
                "channels": channels,
            }
        )
        truth = dict(truth_by_id[raw_id])
        truth["raw_id"] = raw_id
        truth["id"] = conditioned_id
        conditioned_truth.append(truth)
        ledger.append(
            {
                "raw_id": raw_id,
                "raw_sha256": row["sha256"],
                "conditioned_id": conditioned_id,
                "conditioned_sha256": conditioned_sha256,
                "duration_s": round(duration_s, 3),
            }
        )

    conditioned_candidate_manifest = output_dir / "candidate_manifest.json"
    conditioned_private_manifest = private_dir / "truth_manifest.json"
    conditioned_candidate_manifest.write_text(
        json.dumps(conditioned_candidates, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    conditioned_private_manifest.write_text(
        json.dumps(conditioned_truth, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    lock = {
        "schema_version": 1,
        "conditioning_policy": POLICY,
        "policy_source_sha256": policy_source_sha256,
        "clip_count": len(conditioned_candidates),
        "total_duration_s": round(
            sum(float(row["duration_s"]) for row in conditioned_candidates), 3
        ),
        "raw_candidate_manifest_sha256": _sha256_file(candidate_manifest),
        "raw_private_truth_manifest_sha256": _sha256_file(private_truth_manifest),
        "candidate_manifest_sha256": _sha256_file(conditioned_candidate_manifest),
        "private_truth_manifest_sha256": _sha256_file(conditioned_private_manifest),
        "conditioning_ledger": ledger,
    }
    (output_dir / "corpus-lock.json").write_text(
        json.dumps(lock, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return ConditionResult(
        len(conditioned_candidates),
        conditioned_candidate_manifest,
        conditioned_private_manifest,
    )


def production_conditioner(
    ffmpeg: Path, condition_audio: Path
) -> Callable[[Path, Path], None]:
    def condition(source: Path, destination: Path) -> None:
        decoded = subprocess.run(
            [
                str(ffmpeg),
                "-v",
                "error",
                "-i",
                str(source),
                "-f",
                "f32le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-",
            ],
            check=True,
            stdout=subprocess.PIPE,
        ).stdout
        conditioned = subprocess.run(
            [str(condition_audio), "16000", POLICY],
            check=True,
            input=decoded,
            stdout=subprocess.PIPE,
        ).stdout
        subprocess.run(
            [
                str(ffmpeg),
                "-v",
                "error",
                "-f",
                "f32le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-i",
                "-",
                "-c:a",
                "pcm_s16le",
                "-y",
                str(destination),
            ],
            check=True,
            input=conditioned,
        )

    return condition


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Apply HeardRight's production ASR conditioning to a locked corpus."
    )
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--truth", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--ffmpeg", required=True, type=Path)
    parser.add_argument("--condition-audio", required=True, type=Path)
    parser.add_argument("--policy-source", required=True, type=Path)
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()
    result = condition_locked_corpus(
        candidate_manifest=args.candidate,
        private_truth_manifest=args.truth,
        output_dir=args.output,
        conditioner=production_conditioner(args.ffmpeg, args.condition_audio),
        policy_source_sha256=_sha256_file(args.policy_source),
        limit=args.limit,
    )
    print(
        json.dumps(
            {
                "clip_count": result.clip_count,
                "candidate_manifest": str(result.candidate_manifest.resolve()),
                "private_manifest": str(result.private_manifest.resolve()),
            }
        )
    )


if __name__ == "__main__":
    main()
