from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import re
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence

import pyarrow.parquet as pq


PRODUCTIVITY_TERMS = re.compile(
    r"\b(?:message|messages|text|texts|reply|respond|send|email|mail|tell|write|"
    r"remind|reminder|calendar|schedule|appointment|meeting|note|notes|memo|"
    r"list|dictate|dictation|paragraph|bullet|markdown)\b",
    re.IGNORECASE,
)
COMMAND_MARKERS = re.compile(
    r"\b(?:hey facebook|send (?:a )?(?:text|message)|send text|send message|"
    r"text my|reply(?: to)?|respond(?: to)?|end (?:text|message)|"
    r"lock phone|unlock phone)\b",
    re.IGNORECASE,
)
INSTRUCTIONAL_SPEECH = re.compile(
    r"\b(?:another way|within the messaging app|how (?:do|would|can) (?:i|you)|"
    r"you (?:can|could|would)|using (?:the|your) messaging app|"
    r"to text the (?:group|person)|first add friends|press send|type in the message|"
    r"open (?:the|your|my|our) messaging app|add friends to a group chat|"
    r"once sent the message|go to the contact list|look up contacts|"
    r"now using (?:the|your) (?:main )?messaging app|i would text one message|"
    r"i can send to them my text)\b",
    re.IGNORECASE,
)
MEDIA_COMMAND = re.compile(
    r"\b(?:photo|picture|video|song|music|playlist|album|volume|battery|notification)\b",
    re.IGNORECASE,
)
WORD_BUCKETS = (
    (7, 15, 0.25),
    (16, 30, 0.375),
    (31, 60, 0.30),
    (61, 90, 0.075),
)


@dataclass(frozen=True)
class BuildResult:
    clip_count: int
    candidate_manifest: Path
    private_manifest: Path


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _audio_filename(source_name: str) -> str:
    return source_name if source_name.lower().endswith(".wav") else f"{source_name}.wav"


def _productivity_score(transcription: str) -> tuple[int, int]:
    words = transcription.split()
    term_count = len(PRODUCTIVITY_TERMS.findall(transcription))
    command_count = len(COMMAND_MARKERS.findall(transcription))
    has_payload = (
        7 <= len(words) <= 90
        and term_count > 0
        and command_count <= 2
        and not INSTRUCTIONAL_SPEECH.search(transcription)
        and not MEDIA_COMMAND.search(transcription)
    )
    return (1 if has_payload else 0, min(len(words), 80) + term_count * 8)


def select_productivity_rows(
    rows: Sequence[Mapping[str, str]], count: int
) -> list[Mapping[str, str]]:
    seen: set[str] = set()
    for row in rows:
        audio_id = row["hash_name"]
        if audio_id in seen:
            raise ValueError(f"duplicate audio id: {audio_id}")
        seen.add(audio_id)

    def rank_key(row: Mapping[str, str]) -> tuple[int, str]:
        return (-_productivity_score(row["transcription"])[1], row["hash_name"])

    eligible = [row for row in rows if _productivity_score(row["transcription"])[0]]
    if len(eligible) < count:
        raise ValueError(
            f"requested {count} productivity clips but only {len(eligible)} qualified"
        )

    raw_quotas = [count * share for _, _, share in WORD_BUCKETS]
    quotas = [int(raw) for raw in raw_quotas]
    remaining_quota = count - sum(quotas)
    remainder_order = sorted(
        range(len(WORD_BUCKETS)),
        key=lambda index: (-(raw_quotas[index] - quotas[index]), index),
    )
    for index in remainder_order[:remaining_quota]:
        quotas[index] += 1

    selected: list[Mapping[str, str]] = []
    selected_ids: set[str] = set()
    for (minimum, maximum, _), quota in zip(WORD_BUCKETS, quotas):
        bucket = sorted(
            (
                row
                for row in eligible
                if minimum <= len(row["transcription"].split()) <= maximum
            ),
            key=rank_key,
        )
        for row in bucket[:quota]:
            selected.append(row)
            selected_ids.add(row["hash_name"])

    if len(selected) < count:
        unselected = sorted(
            (row for row in eligible if row["hash_name"] not in selected_ids),
            key=rank_key,
        )
        selected.extend(unselected[: count - len(selected)])
    return selected


def _inspect_wav(data: bytes) -> tuple[int, int, float]:
    with wave.open(io.BytesIO(data), "rb") as wav:
        channels = wav.getnchannels()
        sample_rate = wav.getframerate()
        sample_width = wav.getsampwidth()
        frame_count = wav.getnframes()
    if channels != 1 or sample_rate != 16000 or sample_width != 2:
        raise ValueError(
            "audio must be mono PCM s16le at 16 kHz; "
            f"got channels={channels}, rate={sample_rate}, width={sample_width}"
        )
    return channels, sample_rate, frame_count / sample_rate


def build_locked_corpus(
    rows: Sequence[Mapping[str, str]],
    audio_by_name: Mapping[str, bytes],
    output_dir: Path,
) -> BuildResult:
    audio_dir = output_dir / "candidate_audio"
    private_dir = output_dir / "_scorer_private"
    audio_dir.mkdir(parents=True, exist_ok=True)
    private_dir.mkdir(parents=True, exist_ok=True)

    candidate_rows: list[dict[str, object]] = []
    private_rows: list[dict[str, object]] = []
    for row in rows:
        source_name = row["hash_name"]
        audio_filename = _audio_filename(source_name)
        try:
            audio = audio_by_name[audio_filename]
        except KeyError as exc:
            raise ValueError(f"missing audio: {audio_filename}") from exc
        channels, sample_rate, duration_s = _inspect_wav(audio)
        digest = hashlib.sha256(audio).hexdigest()
        clip_id = digest[:16]
        output_path = (audio_dir / f"{clip_id}.wav").resolve()
        output_path.write_bytes(audio)
        candidate_rows.append(
            {
                "id": clip_id,
                "path": str(output_path),
                "duration_s": round(duration_s, 3),
                "sha256": digest,
                "sample_rate_hz": sample_rate,
                "channels": channels,
            }
        )
        private_rows.append(
            {
                "id": clip_id,
                "source_name": source_name,
                "transcription": row["transcription"],
                "demographics": {
                    key: row.get(key, "")
                    for key in (
                        "age",
                        "gender",
                        "first_language",
                        "socioeconomic_bkgd",
                        "ethnicity",
                    )
                },
            }
        )

    candidate_manifest = output_dir / "candidate_manifest.json"
    private_manifest = private_dir / "truth_manifest.json"
    candidate_manifest.write_text(
        json.dumps(candidate_rows, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    private_manifest.write_text(
        json.dumps(private_rows, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return BuildResult(len(candidate_rows), candidate_manifest, private_manifest)


def build_from_sources(
    metadata_path: Path,
    parquet_dir: Path,
    output_dir: Path,
    count: int = 224,
) -> BuildResult:
    with metadata_path.open(encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle, delimiter="\t")
        required_fields = {
            "hash_name",
            "transcription",
            "age",
            "gender",
            "first_language",
            "socioeconomic_bkgd",
            "ethnicity",
        }
        if not reader.fieldnames or not required_fields.issubset(reader.fieldnames):
            raise ValueError("metadata TSV is missing required columns")
        rows = list(reader)

    selected = select_productivity_rows(rows, count=count)
    selected_names = {_audio_filename(row["hash_name"]) for row in selected}
    parquet_paths = sorted(parquet_dir.glob("*.parquet"))
    if not parquet_paths:
        raise ValueError(f"no Parquet shards found in {parquet_dir}")

    audio_by_name: dict[str, bytes] = {}
    shard_rows: list[dict[str, object]] = []
    for parquet_path in parquet_paths:
        parquet_file = pq.ParquetFile(parquet_path)
        try:
            shard_rows.append(
                {
                    "name": parquet_path.name,
                    "bytes": parquet_path.stat().st_size,
                    "rows": parquet_file.metadata.num_rows,
                    "sha256": _sha256_file(parquet_path),
                }
            )
            for batch in parquet_file.iter_batches(columns=["audio"], batch_size=100):
                for audio in batch.column(0).to_pylist():
                    source_name = audio["path"]
                    if source_name not in selected_names:
                        continue
                    if source_name in audio_by_name:
                        raise ValueError(
                            f"duplicate audio in Parquet shards: {source_name}"
                        )
                    audio_by_name[source_name] = audio["bytes"]
        finally:
            parquet_file.close()

    missing = sorted(selected_names - audio_by_name.keys())
    if missing:
        raise ValueError(f"missing {len(missing)} selected audio clips: {missing[:3]}")

    result = build_locked_corpus(selected, audio_by_name, output_dir)
    private_review = output_dir / "_scorer_private" / "selection-review.tsv"
    with private_review.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(("source_name", "word_count", "transcription"))
        for row in selected:
            writer.writerow(
                (
                    row["hash_name"],
                    len(row["transcription"].split()),
                    row["transcription"],
                )
            )

    candidate_rows = json.loads(result.candidate_manifest.read_text(encoding="utf-8"))
    lock = {
        "schema_version": 1,
        "selection_version": "fair-speech-productivity-v1",
        "source_dataset": {
            "name": "Fair-Speech",
            "mirror": "https://huggingface.co/datasets/sdialog/voices-asrfairness",
            "official": "https://ai.meta.com/datasets/speech-fairness-dataset/",
            "mirror_rows": len(rows),
        },
        "selection": {
            "count": count,
            "word_buckets": [
                {"min": minimum, "max": maximum, "share": share}
                for minimum, maximum, share in WORD_BUCKETS
            ],
            "selected_source_names_sha256": hashlib.sha256(
                "\n".join(row["hash_name"] for row in selected).encode("utf-8")
            ).hexdigest(),
        },
        "clip_count": result.clip_count,
        "total_duration_s": round(
            sum(float(row["duration_s"]) for row in candidate_rows), 3
        ),
        "source_metadata": {
            "name": metadata_path.name,
            "rows": len(rows),
            "bytes": metadata_path.stat().st_size,
            "sha256": _sha256_file(metadata_path),
        },
        "source_shards": shard_rows,
        "candidate_manifest_sha256": _sha256_file(result.candidate_manifest),
        "private_truth_manifest_sha256": _sha256_file(result.private_manifest),
    }
    (output_dir / "corpus-lock.json").write_text(
        json.dumps(lock, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return result


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Build a locked HeardRight productivity corpus from Fair-Speech."
    )
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--parquet-dir", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--count", type=int, default=224)
    args = parser.parse_args()
    result = build_from_sources(
        metadata_path=args.metadata,
        parquet_dir=args.parquet_dir,
        output_dir=args.output,
        count=args.count,
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
