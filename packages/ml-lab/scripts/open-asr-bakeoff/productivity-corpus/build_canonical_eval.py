from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import statistics
import wave
from collections import Counter
from pathlib import Path
from typing import NamedTuple


LEGACY_GROUPS = ("scripts_md", "podcast_fillers")
PRODUCTIVITY_GROUP = "productivity_fair_speech"
# Long-form dictation clips appended by add_long_form_clips.py. Kept as its own
# group so per-group results from the original 224-clip set stay comparable.
LONG_FORM_GROUP = "productivity_long_form"
# Synthesised long-form dictation (VoiceRight TTS) added by add_tts_long_form.py.
TTS_LONG_FORM_GROUP = "dictation_tts_long_form"
CANDIDATE_KEYS = {
    "id",
    "path",
    "duration_s",
    "sha256",
    "sample_rate_hz",
    "channels",
}
SUPERSEDED_EVAL_PATHS = (
    ".cache/asr-eval/canonical-smoke-20260718",
    ".cache/asr-eval/heardright-productivity-v1",
    ".cache/asr-eval/heardright-productivity-v1-conditioned-20260718",
    ".cache/asr-eval/heardright-productivity-v1-conditioned-smoke2",
    ".cache/asr-eval/heardright-productivity-v1-locked-20260718",
    ".cache/asr-eval/locked-pico-eval",
    ".cache/asr-eval/pico-conditioned-asr_simple_gain_hpf",
    "tauri-app-next/.cache/asr-eval/20260627-heldout-local-d",
    "tauri-app-next/scripts/open-asr-bakeoff/candidate_audio",
    "tauri-app-next/scripts/open-asr-bakeoff/candidate_manifest.json",
    "tauri-app-next/scripts/open-asr-bakeoff/candidate_manifest_no_chime.json",
    "tauri-app-next/scripts/open-asr-bakeoff/_scorer_private",
)


class BuildResult(NamedTuple):
    clip_count: int
    candidate_manifest: Path
    private_truth_manifest: Path
    lock_manifest: Path


def _read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )


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
        sample_rate_hz = wav.getframerate()
        duration_s = wav.getnframes() / sample_rate_hz
    if channels != 1 or sample_width != 2 or sample_rate_hz != 16000:
        raise ValueError(f"audio must be mono PCM s16le at 16 kHz: {path}")
    return channels, sample_rate_hz, duration_s


def _truth_by_id(value) -> dict[str, dict]:
    if isinstance(value, dict):
        rows = {str(key): dict(row) for key, row in value.items()}
    elif isinstance(value, list):
        rows = {str(row["id"]): dict(row) for row in value}
        if len(rows) != len(value):
            raise ValueError("duplicate ids in private truth manifest")
    else:
        raise ValueError("private truth manifest must be a list or object")
    return rows


def select_duration_stratified(
    legacy_rows: list[dict], productivity_rows: list[dict], target_count: int
) -> list[dict]:
    if target_count < len(legacy_rows):
        raise ValueError("target_count is smaller than the kept legacy set")
    needed = target_count - len(legacy_rows)
    if needed > len(productivity_rows):
        raise ValueError("not enough productivity rows to reach target_count")
    if needed == 0:
        return list(legacy_rows)

    ordered = sorted(
        productivity_rows, key=lambda row: (float(row["duration_s"]), row["id"])
    )
    if needed == 1:
        selected = [ordered[len(ordered) // 2]]
    else:
        indices = [
            index * (len(ordered) - 1) // (needed - 1) for index in range(needed)
        ]
        selected = [ordered[index] for index in indices]
    return list(legacy_rows) + selected


def _duration_bins(durations: list[float]) -> dict[str, int]:
    bins = Counter()
    for duration in durations:
        if duration < 5:
            bins["under_5s"] += 1
        elif duration < 10:
            bins["5_to_under_10s"] += 1
        elif duration < 20:
            bins["10_to_under_20s"] += 1
        elif duration < 30:
            bins["20_to_under_30s"] += 1
        else:
            bins["30s_and_over"] += 1
    return dict(sorted(bins.items()))


def build_canonical_eval(
    *,
    legacy_candidate: Path,
    legacy_truth: Path,
    productivity_candidate: Path,
    productivity_truth: Path,
    output_dir: Path,
    target_count: int = 224,
    legacy_limit_per_group: int | None = None,
) -> BuildResult:
    legacy_candidate_rows = _read_json(legacy_candidate)
    productivity_candidate_rows = _read_json(productivity_candidate)
    legacy_truth_rows = _truth_by_id(_read_json(legacy_truth))
    productivity_truth_rows = _truth_by_id(_read_json(productivity_truth))

    kept_legacy: list[dict] = []
    per_group = Counter()
    for row in legacy_candidate_rows:
        truth = legacy_truth_rows.get(row["id"])
        if truth is None:
            raise ValueError(f"legacy candidate id missing from truth: {row['id']}")
        group = truth.get("group")
        if group not in LEGACY_GROUPS:
            continue
        if legacy_limit_per_group is not None and per_group[group] >= legacy_limit_per_group:
            continue
        kept_legacy.append({**row, "_group": group, "_truth": truth})
        per_group[group] += 1

    if any(group not in per_group for group in LEGACY_GROUPS):
        raise ValueError("legacy corpus is missing an approved group")

    new_rows: list[dict] = []
    for row in productivity_candidate_rows:
        truth = productivity_truth_rows.get(row["id"])
        if truth is None:
            raise ValueError(f"productivity candidate id missing from truth: {row['id']}")
        new_rows.append({**row, "_group": PRODUCTIVITY_GROUP, "_truth": truth})

    selected = select_duration_stratified(kept_legacy, new_rows, target_count)
    selected.sort(key=lambda row: row["id"])

    output_dir = output_dir.resolve()
    audio_dir = output_dir / "candidate_audio"
    private_dir = output_dir / "_scorer_private"
    audio_dir.mkdir(parents=True, exist_ok=True)
    private_dir.mkdir(parents=True, exist_ok=True)

    candidates: list[dict[str, object]] = []
    private: dict[str, dict[str, object]] = {}
    expected_audio: set[Path] = set()
    seen_ids: set[str] = set()
    for row in selected:
        source = Path(row["path"]).resolve()
        if not source.is_file():
            raise ValueError(f"source audio does not exist: {source}")
        digest = _sha256_file(source)
        clip_id = str(row["id"])
        if digest != row["sha256"] or digest[:16] != clip_id:
            raise ValueError(f"source audio hash/id mismatch: {source}")
        if clip_id in seen_ids:
            raise ValueError(f"duplicate selected id: {clip_id}")
        seen_ids.add(clip_id)
        channels, sample_rate_hz, duration_s = _inspect_wav(source)
        destination = audio_dir / f"{clip_id}.wav"
        expected_audio.add(destination)
        if not destination.exists() or _sha256_file(destination) != digest:
            shutil.copy2(source, destination)

        rounded_duration = round(duration_s, 3)
        candidates.append(
            {
                "id": clip_id,
                "path": str(destination.resolve()),
                "duration_s": rounded_duration,
                "sha256": digest,
                "sample_rate_hz": sample_rate_hz,
                "channels": channels,
            }
        )
        truth = row["_truth"]
        text = truth.get("ground_truth", truth.get("transcription"))
        if not isinstance(text, str) or not text.strip():
            raise ValueError(f"missing ground truth text for id: {clip_id}")
        private[clip_id] = {
            "clip_id": clip_id,
            "ground_truth": text,
            "group": row["_group"],
            "duration_s": rounded_duration,
            "source": (
                "fair_speech"
                if row["_group"] == PRODUCTIVITY_GROUP
                else "legacy_heardright_eval"
            ),
        }

    for stale in audio_dir.glob("*.wav"):
        if stale not in expected_audio:
            stale.unlink()

    candidate_manifest = output_dir / "candidate_manifest.json"
    private_truth_manifest = private_dir / "id_to_truth.json"
    lock_manifest = output_dir / "corpus-lock.json"
    _write_json(candidate_manifest, candidates)
    _write_json(private_truth_manifest, private)

    durations = [float(row["duration_s"]) for row in candidates]
    group_counts = Counter(row["group"] for row in private.values())
    lock = {
        "schema_version": 1,
        "name": "heardright_productivity_dictation_v1",
        "status": "canonical_future_asr_eval",
        "clip_count": len(candidates),
        "group_counts": dict(sorted(group_counts.items())),
        "selection": {
            "legacy_groups_kept": list(LEGACY_GROUPS),
            "legacy_groups_removed": "all others",
            "productivity_group": PRODUCTIVITY_GROUP,
            "productivity_strategy": "evenly_spaced_across_duration_sorted_rows",
            "target_count": target_count,
        },
        "duration": {
            "total_s": round(sum(durations), 3),
            "min_s": min(durations),
            "median_s": round(statistics.median(durations), 3),
            "max_s": max(durations),
            "bins": _duration_bins(durations),
        },
        "source_manifests": {
            "legacy_candidate_sha256": _sha256_file(legacy_candidate),
            "legacy_truth_sha256": _sha256_file(legacy_truth),
            "productivity_candidate_sha256": _sha256_file(productivity_candidate),
            "productivity_truth_sha256": _sha256_file(productivity_truth),
        },
        "candidate_manifest_sha256": _sha256_file(candidate_manifest),
        "private_truth_manifest_sha256": _sha256_file(private_truth_manifest),
    }
    _write_json(lock_manifest, lock)
    validate_canonical_eval(output_dir)
    return BuildResult(
        len(candidates), candidate_manifest, private_truth_manifest, lock_manifest
    )


def validate_canonical_eval(output_dir: Path) -> dict:
    output_dir = output_dir.resolve()
    candidate_manifest = output_dir / "candidate_manifest.json"
    private_truth_manifest = output_dir / "_scorer_private" / "id_to_truth.json"
    lock_manifest = output_dir / "corpus-lock.json"
    candidates = _read_json(candidate_manifest)
    private = _truth_by_id(_read_json(private_truth_manifest))
    lock = _read_json(lock_manifest)

    if len(candidates) != lock["clip_count"]:
        raise ValueError("candidate count does not match corpus lock")
    candidate_ids = [row["id"] for row in candidates]
    if len(candidate_ids) != len(set(candidate_ids)):
        raise ValueError("duplicate candidate ids")
    if set(candidate_ids) != set(private):
        raise ValueError("candidate/private truth id mismatch")
    if any(set(row) != CANDIDATE_KEYS for row in candidates):
        raise ValueError("candidate manifest contains unexpected or sensitive fields")
    # Mojibake guard. Reading these manifests with a bare open() on Windows decodes
    # UTF-8 as cp1252 and turns a typographic apostrophe into "â€™"; writing back as
    # UTF-8 then bakes the corruption into the ground truth, where it silently
    # inflates every future WER. Cheap to check, expensive to discover later.
    corrupted = sorted(
        clip_id
        for clip_id, row in private.items()
        if any(marker in row.get("ground_truth", "") for marker in ("â€", "Ã©", "�"))
    )
    if corrupted:
        raise ValueError(
            f"ground truth contains mojibake / replacement chars: {corrupted[:5]}"
        )

    if set(row["group"] for row in private.values()) - {
        *LEGACY_GROUPS,
        PRODUCTIVITY_GROUP,
        LONG_FORM_GROUP,
        TTS_LONG_FORM_GROUP,
    }:
        raise ValueError("private truth contains a disallowed group")

    audio_dir = (output_dir / "candidate_audio").resolve()
    for row in candidates:
        audio = Path(row["path"]).resolve()
        if audio.parent != audio_dir or audio.name != f'{row["id"]}.wav':
            raise ValueError(f"candidate path is not opaque and contained: {audio}")
        if _sha256_file(audio) != row["sha256"]:
            raise ValueError(f"candidate audio hash mismatch: {audio}")
        channels, sample_rate_hz, duration_s = _inspect_wav(audio)
        if channels != row["channels"] or sample_rate_hz != row["sample_rate_hz"]:
            raise ValueError(f"candidate audio format metadata mismatch: {audio}")
        if round(duration_s, 3) != float(row["duration_s"]):
            raise ValueError(f"candidate audio duration mismatch: {audio}")

    if _sha256_file(candidate_manifest) != lock["candidate_manifest_sha256"]:
        raise ValueError("candidate manifest hash does not match corpus lock")
    if _sha256_file(private_truth_manifest) != lock["private_truth_manifest_sha256"]:
        raise ValueError("private truth hash does not match corpus lock")
    return lock


def retire_superseded_eval(
    *, repo_root: Path, canonical_dir: Path, expected_clip_count: int = 224
) -> list[Path]:
    repo_root = repo_root.resolve()
    canonical_dir = canonical_dir.resolve()
    if repo_root not in canonical_dir.parents:
        raise ValueError("canonical corpus must be inside the repository root")
    lock = validate_canonical_eval(canonical_dir)
    if lock["clip_count"] != expected_clip_count:
        raise ValueError(
            f"canonical corpus must contain {expected_clip_count} clips before retirement"
        )

    targets = [(repo_root / relative).resolve() for relative in SUPERSEDED_EVAL_PATHS]
    for target in targets:
        if repo_root not in target.parents:
            raise ValueError(f"retirement target is outside repository: {target}")
        if target == canonical_dir or target in canonical_dir.parents:
            raise ValueError(f"retirement target could remove canonical corpus: {target}")

    retired: list[Path] = []
    for target in targets:
        if target.is_dir():
            shutil.rmtree(target)
            retired.append(target)
        elif target.exists():
            target.unlink()
            retired.append(target)
    remaining = [target for target in retired if target.exists()]
    if remaining:
        raise RuntimeError(f"failed to retire superseded paths: {remaining}")
    validate_canonical_eval(canonical_dir)
    return retired


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Build HeardRight's canonical productivity-dictation ASR eval set."
    )
    parser.add_argument("--legacy-candidate", required=True, type=Path)
    parser.add_argument("--legacy-truth", required=True, type=Path)
    parser.add_argument("--productivity-candidate", required=True, type=Path)
    parser.add_argument("--productivity-truth", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target-count", type=int, default=224)
    parser.add_argument("--legacy-limit-per-group", type=int)
    args = parser.parse_args()
    result = build_canonical_eval(
        legacy_candidate=args.legacy_candidate,
        legacy_truth=args.legacy_truth,
        productivity_candidate=args.productivity_candidate,
        productivity_truth=args.productivity_truth,
        output_dir=args.output,
        target_count=args.target_count,
        legacy_limit_per_group=args.legacy_limit_per_group,
    )
    lock = validate_canonical_eval(args.output)
    print(
        json.dumps(
            {
                "clip_count": result.clip_count,
                "candidate_manifest": str(result.candidate_manifest.resolve()),
                "private_truth_manifest": str(result.private_truth_manifest.resolve()),
                "corpus_lock": str(result.lock_manifest.resolve()),
                "group_counts": lock["group_counts"],
                "duration": lock["duration"],
            }
        )
    )


if __name__ == "__main__":
    main()
