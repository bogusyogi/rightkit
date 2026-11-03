"""Add long-form dictation clips to the canonical HeardRight ASR eval corpus.

The canonical corpus (224 clips) is dominated by short utterances: median 12.8 s,
only 11 clips at 30 s or longer. Long-form dictation exercises windowing, overlap
trimming, and drift, which short clips cannot reach.

This script selects additional Fair-Speech clips that are both long and
dictation-shaped, applies the same shipped `asr_simple_gain_hpf` conditioning the
locked corpus already uses, and appends them under a separate
`productivity_long_form` group so existing per-group results stay comparable.

Selection is deterministic:

* duration at or above `--min-duration` seconds,
* at least `MIN_DICTATION_TERMS` dictation/messaging terms in the transcript,
* media-command terms must not dominate the dictation terms,
* app-operation instructional speech is rejected,
* repetitive command drilling is rejected via a unique-token ratio floor,
* the kept rows are then sampled at evenly spaced *durations* (not ranks), so the
  result spans the available length range instead of clustering at the mode.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import statistics
import subprocess
import tempfile
import wave
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Sequence

import pyarrow.parquet as pq


LONG_FORM_GROUP = "productivity_long_form"
CONDITIONING_POLICY = "asr_simple_gain_hpf"
MIN_DICTATION_TERMS = 3
MIN_UNIQUE_TOKEN_RATIO = 0.45

DICTATION_TERMS = re.compile(
    r"\b(?:message|messages|text|texts|reply|respond|send|email|mail|tell|write|"
    r"remind|reminder|calendar|schedule|appointment|meeting|note|notes|memo)\b",
    re.IGNORECASE,
)
MEDIA_TERMS = re.compile(
    r"\b(?:song|songs|music|playlist|album|albums|volume|itunes|spotify|"
    r"iheartradio|radio|video|photo|picture)\b",
    re.IGNORECASE,
)
INSTRUCTIONAL_SPEECH = re.compile(
    r"\b(?:another way|other ways|you (?:can|could|would)|"
    r"how (?:do|would|can) (?:i|you))\b",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class Candidate:
    source_name: str
    transcription: str
    duration_s: float


@dataclass(frozen=True)
class AddResult:
    added: int
    clip_count: int
    candidate_manifest: Path
    private_truth_manifest: Path
    lock_manifest: Path


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )


def _inspect_wav(path: Path) -> tuple[int, int, float]:
    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        sample_width = wav.getsampwidth()
        sample_rate_hz = wav.getframerate()
        duration_s = wav.getnframes() / sample_rate_hz
    if channels != 1 or sample_width != 2 or sample_rate_hz != 16000:
        raise ValueError(f"audio must be mono PCM s16le at 16 kHz: {path}")
    return channels, sample_rate_hz, duration_s


def is_long_form_dictation(transcription: str) -> bool:
    """Reject media commands, app-operation instructions, and command drilling."""
    tokens = transcription.split()
    if not tokens:
        return False
    if INSTRUCTIONAL_SPEECH.search(transcription):
        return False
    dictation = len(DICTATION_TERMS.findall(transcription))
    if dictation < MIN_DICTATION_TERMS:
        return False
    if len(MEDIA_TERMS.findall(transcription)) * 3 > dictation:
        return False
    unique_ratio = len({token.lower() for token in tokens}) / len(tokens)
    return unique_ratio >= MIN_UNIQUE_TOKEN_RATIO


def select_duration_spread(
    candidates: Sequence[Candidate], count: int
) -> list[Candidate]:
    """Pick `count` clips at evenly spaced target durations across the range.

    Rank-spacing clusters wherever the source pool is dense; duration-spacing is
    what actually produces "a few clips at different lengths".
    """
    if count <= 0:
        raise ValueError("count must be positive")
    if count > len(candidates):
        raise ValueError(
            f"requested {count} long-form clips but only {len(candidates)} qualified"
        )
    ordered = sorted(candidates, key=lambda row: (row.duration_s, row.source_name))
    if count == 1:
        return [ordered[len(ordered) // 2]]

    shortest = ordered[0].duration_s
    longest = ordered[-1].duration_s
    step = (longest - shortest) / (count - 1)
    remaining = list(ordered)
    selected: list[Candidate] = []
    for index in range(count):
        target = shortest + step * index
        best = min(
            remaining,
            key=lambda row: (abs(row.duration_s - target), row.source_name),
        )
        remaining.remove(best)
        selected.append(best)
    return sorted(selected, key=lambda row: (row.duration_s, row.source_name))


def load_source_pool(metadata_path: Path, parquet_dir: Path) -> list[Candidate]:
    with metadata_path.open(encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle, delimiter="\t")
        if not reader.fieldnames or not {
            "hash_name",
            "transcription",
        }.issubset(reader.fieldnames):
            raise ValueError("metadata TSV is missing required columns")
        transcripts = {row["hash_name"]: row["transcription"] for row in reader}

    parquet_paths = sorted(parquet_dir.glob("*.parquet"))
    if not parquet_paths:
        raise ValueError(f"no Parquet shards found in {parquet_dir}")

    pool: list[Candidate] = []
    for parquet_path in parquet_paths:
        parquet_file = pq.ParquetFile(parquet_path)
        try:
            for batch in parquet_file.iter_batches(columns=["audio"], batch_size=200):
                for audio in batch.column(0).to_pylist():
                    name = audio["path"]
                    key = name[:-4] if name.lower().endswith(".wav") else name
                    transcription = transcripts.get(key)
                    if transcription is None:
                        continue
                    # 16 kHz mono s16le: 32000 bytes per second after the 44-byte header.
                    duration_s = (len(audio["bytes"]) - 44) / 32000.0
                    pool.append(Candidate(key, transcription, duration_s))
        finally:
            parquet_file.close()
    return pool


def read_audio_bytes(
    parquet_dir: Path, wanted: Iterable[str]
) -> dict[str, bytes]:
    remaining = set(wanted)
    found: dict[str, bytes] = {}
    for parquet_path in sorted(parquet_dir.glob("*.parquet")):
        if not remaining:
            break
        parquet_file = pq.ParquetFile(parquet_path)
        try:
            for batch in parquet_file.iter_batches(columns=["audio"], batch_size=200):
                for audio in batch.column(0).to_pylist():
                    name = audio["path"]
                    key = name[:-4] if name.lower().endswith(".wav") else name
                    if key in remaining:
                        found[key] = audio["bytes"]
                        remaining.discard(key)
        finally:
            parquet_file.close()
    if remaining:
        raise ValueError(f"missing audio for: {sorted(remaining)}")
    return found


def condition_bytes(
    raw: bytes, destination: Path, ffmpeg: Path, condition_audio: Path
) -> None:
    """Byte-for-byte the pipeline that produced the locked corpus."""
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(temp_dir) / "source.wav"
        source.write_bytes(raw)
        decoded = subprocess.run(
            [
                str(ffmpeg), "-v", "error", "-i", str(source),
                "-f", "f32le", "-ar", "16000", "-ac", "1", "-",
            ],
            check=True,
            stdout=subprocess.PIPE,
        ).stdout
    conditioned = subprocess.run(
        [str(condition_audio), "16000", CONDITIONING_POLICY],
        check=True,
        input=decoded,
        stdout=subprocess.PIPE,
    ).stdout
    destination.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            str(ffmpeg), "-v", "error",
            "-f", "f32le", "-ar", "16000", "-ac", "1", "-i", "-",
            "-c:a", "pcm_s16le", "-y", str(destination),
        ],
        check=True,
        input=conditioned,
    )


def _duration_bins(durations: Sequence[float]) -> dict[str, int]:
    bins: Counter[str] = Counter()
    for duration in durations:
        if duration < 5:
            bins["under_5s"] += 1
        elif duration < 10:
            bins["5_to_under_10s"] += 1
        elif duration < 20:
            bins["10_to_under_20s"] += 1
        elif duration < 30:
            bins["20_to_under_30s"] += 1
        elif duration < 60:
            bins["30_to_under_60s"] += 1
        else:
            bins["60s_and_over"] += 1
    return dict(sorted(bins.items()))


def add_long_form_clips(
    *,
    canonical_dir: Path,
    metadata_path: Path,
    parquet_dir: Path,
    ffmpeg: Path,
    condition_audio: Path,
    policy_source: Path,
    count: int,
    min_duration_s: float,
) -> AddResult:
    canonical_dir = canonical_dir.resolve()
    candidate_manifest = canonical_dir / "candidate_manifest.json"
    private_truth_manifest = canonical_dir / "_scorer_private" / "id_to_truth.json"
    lock_manifest = canonical_dir / "corpus-lock.json"

    candidates = _read_json(candidate_manifest)
    private = _read_json(private_truth_manifest)
    lock = _read_json(lock_manifest)

    existing_shas = {row["sha256"] for row in candidates}
    existing_ids = {row["id"] for row in candidates}

    pool = load_source_pool(metadata_path, parquet_dir)
    eligible = [
        row
        for row in pool
        if row.duration_s >= min_duration_s and is_long_form_dictation(row.transcription)
    ]
    selected = select_duration_spread(eligible, count)
    audio_by_name = read_audio_bytes(parquet_dir, (row.source_name for row in selected))

    audio_dir = canonical_dir / "candidate_audio"
    added_rows: list[dict[str, object]] = []
    for row in selected:
        with tempfile.TemporaryDirectory() as temp_dir:
            staged = Path(temp_dir) / "conditioned.wav"
            condition_bytes(
                audio_by_name[row.source_name], staged, ffmpeg, condition_audio
            )
            digest = hashlib.sha256(staged.read_bytes()).hexdigest()
            clip_id = digest[:16]
            if digest in existing_shas or clip_id in existing_ids:
                raise ValueError(f"clip already present in the corpus: {clip_id}")
            existing_shas.add(digest)
            existing_ids.add(clip_id)
            destination = audio_dir / f"{clip_id}.wav"
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(staged.read_bytes())

        channels, sample_rate_hz, duration_s = _inspect_wav(destination)
        rounded = round(duration_s, 3)
        added_rows.append(
            {
                "id": clip_id,
                "path": str(destination),
                "duration_s": rounded,
                "sha256": digest,
                "sample_rate_hz": sample_rate_hz,
                "channels": channels,
            }
        )
        private[clip_id] = {
            "clip_id": clip_id,
            "ground_truth": row.transcription,
            "group": LONG_FORM_GROUP,
            "duration_s": rounded,
            "source": "fair_speech",
        }

    candidates.extend(added_rows)
    candidates.sort(key=lambda row: row["id"])
    private = {key: private[key] for key in sorted(private)}
    _write_json(candidate_manifest, candidates)
    _write_json(private_truth_manifest, private)

    durations = [float(row["duration_s"]) for row in candidates]
    group_counts = Counter(row["group"] for row in private.values())
    lock["clip_count"] = len(candidates)
    lock["group_counts"] = dict(sorted(group_counts.items()))
    lock["duration"] = {
        "total_s": round(sum(durations), 3),
        "min_s": min(durations),
        "median_s": round(statistics.median(durations), 3),
        "max_s": max(durations),
        "bins": _duration_bins(durations),
    }
    lock["long_form_addition"] = {
        "group": LONG_FORM_GROUP,
        "added": len(added_rows),
        "min_duration_s": min_duration_s,
        "eligible_pool": len(eligible),
        "strategy": "evenly_spaced_across_duration_range",
        "conditioning_policy": CONDITIONING_POLICY,
        "policy_source_sha256": _sha256_file(policy_source),
        "selected_source_names_sha256": hashlib.sha256(
            "\n".join(row.source_name for row in selected).encode("utf-8")
        ).hexdigest(),
    }
    lock["candidate_manifest_sha256"] = _sha256_file(candidate_manifest)
    lock["private_truth_manifest_sha256"] = _sha256_file(private_truth_manifest)
    _write_json(lock_manifest, lock)

    return AddResult(
        len(added_rows),
        len(candidates),
        candidate_manifest,
        private_truth_manifest,
        lock_manifest,
    )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Add long-form dictation clips to the canonical ASR eval corpus."
    )
    parser.add_argument("--canonical-dir", required=True, type=Path)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--parquet-dir", required=True, type=Path)
    parser.add_argument("--ffmpeg", required=True, type=Path)
    parser.add_argument("--condition-audio", required=True, type=Path)
    parser.add_argument("--policy-source", required=True, type=Path)
    parser.add_argument("--count", type=int, default=8)
    parser.add_argument("--min-duration", type=float, default=40.0)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the selection without writing audio or manifests",
    )
    args = parser.parse_args()

    if args.dry_run:
        pool = load_source_pool(args.metadata, args.parquet_dir)
        eligible = [
            row
            for row in pool
            if row.duration_s >= args.min_duration
            and is_long_form_dictation(row.transcription)
        ]
        selected = select_duration_spread(eligible, args.count)
        print(
            json.dumps(
                {
                    "pool": len(pool),
                    "eligible": len(eligible),
                    "selected": [
                        {
                            "source_name": row.source_name,
                            "duration_s": round(row.duration_s, 3),
                            "words": len(row.transcription.split()),
                            "transcription": row.transcription,
                        }
                        for row in selected
                    ],
                },
                indent=2,
            )
        )
        return

    result = add_long_form_clips(
        canonical_dir=args.canonical_dir,
        metadata_path=args.metadata,
        parquet_dir=args.parquet_dir,
        ffmpeg=args.ffmpeg.resolve(),
        condition_audio=args.condition_audio.resolve(),
        policy_source=args.policy_source,
        count=args.count,
        min_duration_s=args.min_duration,
    )
    print(
        json.dumps(
            {
                "added": result.added,
                "clip_count": result.clip_count,
                "candidate_manifest": str(result.candidate_manifest),
                "private_truth_manifest": str(result.private_truth_manifest),
                "corpus_lock": str(result.lock_manifest),
            }
        )
    )


if __name__ == "__main__":
    main()
