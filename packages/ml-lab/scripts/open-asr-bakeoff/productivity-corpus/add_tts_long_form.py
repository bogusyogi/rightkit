"""Synthesise long-form dictation clips and add them to the canonical ASR eval corpus.

Why synthesis rather than more found audio: every public long-form corpus we have
is speech *performed to a room* — TED talks, parliament, meetings, audiobooks. A
dictation app's acoustic and stylistic domain is one person talking to a device at
desk distance, and no public long-form set matches it. Synthesising the audio also
makes the reference exact, because the script IS the ground truth rather than a
transcription that must itself be trusted.

Pipeline: Chatterbox (VoiceRight's `eval/.venv`) clones a real Fair-Speech speaker
from a 6-10 s reference, each script is synthesised in sentence-grouped chunks with
natural pauses between them, the chunks are concatenated, and the result goes
through the shipped `asr_simple_gain_hpf` conditioning like every other clip.

Read `KNOWN LIMITATION` below before using these clips to compare engines.

KNOWN LIMITATION
    Synthetic speech is distribution-shifted from human speech: no disfluencies,
    no room, no mic variation, cleaner prosody. WER on this group will be
    optimistic and is NOT interchangeable with WER on the human-speech groups.
    Its value is as a long-form *stress* signal — windowing, overlap trimming and
    drift over minutes — measured within the group and across engines, never as an
    absolute accuracy number. It is a separate group so it can never silently
    average into the human-speech results.

Two processes, because Chatterbox needs VoiceRight's venv and the corpus tooling
needs the workspace Python:

    # 1. synthesise (VoiceRight venv, GPU)
    D:/Claude/voiceright/eval/.venv/Scripts/python.exe add_tts_long_form.py synth \
        --texts dictation_long_form_texts.json --out-dir <staging dir>

    # 2. condition + append to the corpus (workspace py -3.11)
    py -3.11 add_tts_long_form.py add --staging <staging dir> \
        --canonical-dir ../canonical-eval --ffmpeg ... --condition-audio ... --policy-source ...
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import statistics
import sys
import tempfile
import wave
from collections import Counter
from pathlib import Path

TTS_LONG_FORM_GROUP = "dictation_tts_long_form"

# Real Fair-Speech speakers used as clone references: 3 male / 3 female, 18-65.
# Speaker diversity comes from real humans rather than from a synthetic voice
# description, which keeps the timbre distribution closer to actual users.
#
# `wpm` is the speaking rate MEASURED from a first synthesis pass on this box.
# Chatterbox inherits pace from the reference, and the spread is wide (135-210),
# so the reference has to be chosen per passage or a long script lands minutes
# short of its target. Re-measure if the references ever change.
REFERENCE_VOICES = {
    "m18": ("00252dd761a6d86b50ef61871b2fe6b7", "male", "18 - 22", 208),
    "m31": ("00257eb0018e0cf7bcd4215a4c63e2d3", "male", "31 - 45", 148),
    "f46": ("0040910ada69bddd06b9027d2c4762dd", "female", "46 - 65", 135),
    "f23": ("004c6d9baa1df3ec266593965f55910c", "female", "23 - 30", 156),
    "f18": ("008f6e115d6d7143ed0186ed409a5055", "female", "18 - 22", 210),
    "m46": ("0098814d0c581511dc58a67e8fb5121c", "male", "46 - 65", 145),
}

# Passage -> reference. Long passages need a slow voice or they land minutes short:
# `product_announcement_draft` first ran on f18 (210 wpm) and produced 3.5 min
# against a 5 min target, so it moved to m31 (148 wpm) -> ~5.0 min. m31 therefore
# appears twice, once on a 1-minute script and once on a 5-minute one; f18 is
# unused. Every other passage kept the voice it was first synthesised with.
PASSAGE_VOICE = {
    "client_followup_email": "m18",
    "bug_voice_note": "m31",
    "weekly_status_update": "f46",
    "planning_meeting_notes": "f23",
    "product_announcement_draft": "m31",
    "technical_design_note": "m46",
}

# Chatterbox degrades on long inputs, so scripts are synthesised in chunks. ~240
# characters keeps each chunk inside the stable range while still breaking on
# sentence boundaries, so the pauses land where a person would actually pause.
MAX_CHUNK_CHARS = 240
PAUSE_S = 0.32
# Measured on this box at realistic chunk length: cfg_weight 0.5 -> 167 wpm,
# 0.3 -> 152 wpm, 0.2 -> 132 wpm. 0.3 lands on the ~150 wpm the scripts are
# written against, so target_minutes and realised duration agree.
CFG_WEIGHT = 0.3


def chunk_text(text: str, max_chars: int = MAX_CHUNK_CHARS) -> list[str]:
    """Group whole sentences into chunks without ever splitting mid-sentence."""
    sentences = [s.strip() for s in re.split(r"(?<=[.?!])\s+", text.strip()) if s.strip()]
    chunks: list[str] = []
    current = ""
    for sentence in sentences:
        if current and len(current) + 1 + len(sentence) > max_chars:
            chunks.append(current)
            current = sentence
        else:
            current = f"{current} {sentence}".strip()
    if current:
        chunks.append(current)
    return chunks


# --------------------------------------------------------------------------
# stage 1: synth (runs under VoiceRight's venv)
# --------------------------------------------------------------------------


def run_synth(
    texts_path: Path,
    out_dir: Path,
    sources_dir: Path,
    only: set[str] | None = None,
) -> None:
    import numpy as np
    import soundfile as sf
    import torch
    from chatterbox.tts import ChatterboxTTS

    if not torch.cuda.is_available():
        raise SystemExit("CPU torch — refusing to synthesise minutes of audio on CPU")
    print(f"cuda: {torch.cuda.get_device_name(0)}", flush=True)

    passages = json.loads(texts_path.read_text(encoding="utf-8"))["passages"]
    out_dir.mkdir(parents=True, exist_ok=True)
    model = ChatterboxTTS.from_pretrained(device="cuda")

    index_path = out_dir / "index.json"
    index: list[dict[str, object]] = (
        json.loads(index_path.read_text(encoding="utf-8")) if index_path.exists() else []
    )
    if only:
        # Keep previously synthesised clips; replace just the named passages.
        index = [row for row in index if row["id"] not in only]
        passages = [p for p in passages if p["id"] in only]
        if not passages:
            raise SystemExit(f"no passages matched --only {sorted(only)}")

    for passage in passages:
        ref_name, gender, age, _wpm = REFERENCE_VOICES[PASSAGE_VOICE[passage["id"]]]
        ref_path = sources_dir / "audio" / f"{ref_name}.wav"
        if not ref_path.is_file():
            raise SystemExit(f"reference audio missing: {ref_path}")

        chunks = chunk_text(passage["text"])
        pause = np.zeros(int(PAUSE_S * model.sr), dtype=np.float32)
        pieces: list[np.ndarray] = []
        for number, chunk in enumerate(chunks, start=1):
            wav = model.generate(
                chunk, audio_prompt_path=str(ref_path), cfg_weight=CFG_WEIGHT
            )
            pieces.append(wav.squeeze(0).detach().cpu().numpy().astype(np.float32))
            if number < len(chunks):
                pieces.append(pause)
            print(f"  [{passage['id']}] chunk {number}/{len(chunks)}", flush=True)

        audio = np.concatenate(pieces)
        destination = out_dir / f"{passage['id']}.wav"
        sf.write(destination, audio, model.sr, subtype="PCM_16")
        duration_s = len(audio) / model.sr
        index.append(
            {
                "id": passage["id"],
                "wav": destination.name,
                "text": passage["text"],
                "target_minutes": passage["target_minutes"],
                "kind": passage["kind"],
                "duration_s": round(duration_s, 3),
                "chunks": len(chunks),
                "reference": {"source_name": ref_name, "gender": gender, "age": age},
            }
        )
        print(
            f"[{passage['id']}] {duration_s:.1f}s "
            f"(target ~{passage['target_minutes']}min) via {gender} {age}",
            flush=True,
        )

    order = {p["id"]: n for n, p in enumerate(
        json.loads(texts_path.read_text(encoding="utf-8"))["passages"]
    )}
    index.sort(key=lambda row: order.get(str(row["id"]), 0))
    index_path.write_text(
        json.dumps(index, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print(f"wrote {len(index)} clips -> {out_dir}", flush=True)


# --------------------------------------------------------------------------
# stage 2: condition + append (runs under the workspace Python)
# --------------------------------------------------------------------------


def _load_helpers():
    spec = importlib.util.spec_from_file_location(
        "add_long_form_clips", Path(__file__).with_name("add_long_form_clips.py")
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    # Register before exec: @dataclass resolves field types via sys.modules.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def run_add(
    *,
    staging: Path,
    canonical_dir: Path,
    ffmpeg: Path,
    condition_audio: Path,
    policy_source: Path,
) -> dict[str, object]:
    helpers = _load_helpers()
    canonical_dir = canonical_dir.resolve()
    candidate_manifest = canonical_dir / "candidate_manifest.json"
    private_truth_manifest = canonical_dir / "_scorer_private" / "id_to_truth.json"
    lock_manifest = canonical_dir / "corpus-lock.json"

    candidates = helpers._read_json(candidate_manifest)
    private = helpers._read_json(private_truth_manifest)
    lock = helpers._read_json(lock_manifest)
    existing_shas = {row["sha256"] for row in candidates}
    existing_ids = {row["id"] for row in candidates}

    index = helpers._read_json(staging / "index.json")
    audio_dir = canonical_dir / "candidate_audio"
    added_rows: list[dict[str, object]] = []
    provenance: list[dict[str, object]] = []

    for entry in index:
        source = staging / entry["wav"]
        if not source.is_file():
            raise ValueError(f"staged audio missing: {source}")
        with tempfile.TemporaryDirectory() as temp_dir:
            staged = Path(temp_dir) / "conditioned.wav"
            helpers.condition_bytes(
                source.read_bytes(), staged, ffmpeg, condition_audio
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

        channels, sample_rate_hz, duration_s = helpers._inspect_wav(destination)
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
            "ground_truth": entry["text"],
            "group": TTS_LONG_FORM_GROUP,
            "duration_s": rounded,
            "source": "chatterbox_tts",
        }
        provenance.append(
            {
                "passage_id": entry["id"],
                "kind": entry["kind"],
                "target_minutes": entry["target_minutes"],
                "duration_s": rounded,
                "words": len(entry["text"].split()),
                "reference_voice": entry["reference"],
                "clip_id": clip_id,
            }
        )

    candidates.extend(added_rows)
    candidates.sort(key=lambda row: row["id"])
    private = {key: private[key] for key in sorted(private)}
    helpers._write_json(candidate_manifest, candidates)
    helpers._write_json(private_truth_manifest, private)

    durations = [float(row["duration_s"]) for row in candidates]
    group_counts = Counter(row["group"] for row in private.values())
    lock["clip_count"] = len(candidates)
    lock["group_counts"] = dict(sorted(group_counts.items()))
    lock["duration"] = {
        "total_s": round(sum(durations), 3),
        "min_s": min(durations),
        "median_s": round(statistics.median(durations), 3),
        "max_s": max(durations),
        "bins": helpers._duration_bins(durations),
    }
    lock["tts_long_form_addition"] = {
        "group": TTS_LONG_FORM_GROUP,
        "added": len(added_rows),
        "engine": "chatterbox (VoiceRight eval venv)",
        "reference_speakers": "real Fair-Speech speakers, 3 male / 3 female, ages 18-65",
        "scripts": "dictation_long_form_texts.json (the script IS the ground truth)",
        "chunking": {"max_chars": MAX_CHUNK_CHARS, "pause_s": PAUSE_S},
        "conditioning_policy": helpers.CONDITIONING_POLICY,
        "policy_source_sha256": helpers._sha256_file(policy_source),
        "caveat": (
            "Synthetic speech is distribution-shifted from human speech; WER here is "
            "optimistic and is not comparable to the human-speech groups. Use it as a "
            "long-form stress signal within the group, not as an absolute accuracy number."
        ),
        "clips": provenance,
    }
    lock["candidate_manifest_sha256"] = helpers._sha256_file(candidate_manifest)
    lock["private_truth_manifest_sha256"] = helpers._sha256_file(private_truth_manifest)
    helpers._write_json(lock_manifest, lock)

    return {
        "added": len(added_rows),
        "clip_count": len(candidates),
        "clips": provenance,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    synth = sub.add_parser("synth", help="synthesise clips (VoiceRight venv, GPU)")
    synth.add_argument("--texts", required=True, type=Path)
    synth.add_argument("--out-dir", required=True, type=Path)
    synth.add_argument(
        "--sources-dir",
        type=Path,
        default=Path(
            "D:/Claude/heardright/.cache/productivity-corpus-sources/fair-speech-hf"
        ),
    )
    synth.add_argument(
        "--only",
        nargs="*",
        help="resynthesise only these passage ids, keeping the rest of index.json",
    )

    add = sub.add_parser("add", help="condition staged clips and append to the corpus")
    add.add_argument("--staging", required=True, type=Path)
    add.add_argument("--canonical-dir", required=True, type=Path)
    add.add_argument("--ffmpeg", required=True, type=Path)
    add.add_argument("--condition-audio", required=True, type=Path)
    add.add_argument("--policy-source", required=True, type=Path)

    args = parser.parse_args()
    if args.command == "synth":
        run_synth(
            args.texts,
            args.out_dir,
            args.sources_dir,
            set(args.only) if args.only else None,
        )
    else:
        result = run_add(
            staging=args.staging,
            canonical_dir=args.canonical_dir,
            ffmpeg=args.ffmpeg.resolve(),
            condition_audio=args.condition_audio.resolve(),
            policy_source=args.policy_source,
        )
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
