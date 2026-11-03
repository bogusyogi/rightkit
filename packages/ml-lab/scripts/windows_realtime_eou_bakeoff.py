#!/usr/bin/env python3
"""Isolated Windows Realtime EOU CPU/DirectML replay; never touches app state."""
from __future__ import annotations

import argparse, hashlib, json, os, subprocess, time
from pathlib import Path
from typing import Any

import numpy as np
import onnxruntime as ort
import soundfile as sf
import sentencepiece as spm

SR = 16000
CADENCE_S = 0.250
EOU = 1024
EOB = 1025
BLANK = 1026
PREFIX_BIAS = 5.0
PHRASES = ("zephyr stop", "zephyr send", "zephyr submit", "zephyr cancel")
QUALIFICATION_SCHEMA = "heardright.windows-eou-qualification-checkpoint.v1"
EXPECTED_CACHE_SHAPES = [[1, 17, 70, 512], [1, 17, 512, 8]]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest().upper()


def probe_offsets(duration_s: float) -> list[float]:
    return [round(index * CADENCE_S, 3) for index in range(int(np.ceil(duration_s / CADENCE_S)))]


class ProductionParser:
    """Persistent JSONL bridge to `heardright_core::parse_control_command`."""
    def __init__(self, helper: Path):
        if not helper.is_file():
            raise RuntimeError(f"production parser helper missing: {helper}")
        self.process = subprocess.Popen([str(helper)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)

    def parse(self, text: str) -> dict[str, Any] | None:
        if self.process.stdin is None or self.process.stdout is None:
            raise RuntimeError("production parser pipes unavailable")
        self.process.stdin.write(json.dumps({"text": text}) + "\n"); self.process.stdin.flush()
        response = json.loads(self.process.stdout.readline())
        if isinstance(response, dict) and "error" in response: raise RuntimeError(f"production parser error: {response['error']}")
        return response

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate(); self.process.wait(timeout=5)
        if self.process.stdin is not None: self.process.stdin.close()
        if self.process.stdout is not None: self.process.stdout.close()

    def __enter__(self) -> "ProductionParser": return self
    def __exit__(self, *_: Any) -> None: self.close()


def providers(kind: str) -> list[str]:
    return ["CPUExecutionProvider"] if kind == "cpu" else ["DmlExecutionProvider", "CPUExecutionProvider"]


def rss_bytes() -> int | None:
    try:
        import psutil  # type: ignore
        return int(psutil.Process().memory_info().rss)
    except Exception:
        return None


def phrase_ids(model: Path) -> tuple[spm.SentencePieceProcessor, tuple[tuple[int, ...], ...]]:
    processor = spm.SentencePieceProcessor(model_file=str(model))
    return processor, tuple(tuple(processor.encode(phrase, out_type=int)) for phrase in PHRASES)


def next_bias(prefix: list[int], phrases: tuple[tuple[int, ...], ...]) -> tuple[int, ...]:
    """Soft trie: only add logits; ordinary greedy RNNT remains unconstrained."""
    expected: set[int] = set()
    for phrase in phrases:
        for length in range(min(len(prefix), len(phrase)), -1, -1):
            if tuple(prefix[-length:]) == phrase[:length] if length else True:
                if length < len(phrase): expected.add(phrase[length])
                break
    return tuple(sorted(expected))


def cache_steps(feature_count: int) -> list[tuple[int, int, int]]:
    """NeMo cache geometry: 9 first, then 9 retained + 16 new frames."""
    if feature_count < 9:
        return []
    result = [(0, 0, 9)]
    index = 9
    while index + 16 <= feature_count:
        result.append((index - 9, 9, 16))
        index += 16
    return result


def normalize_encoder_features(chunk: np.ndarray, valid_len: int) -> np.ndarray:
    """EOU bundle declares `preprocessor.normalize: NA`; preserve raw log-mel."""
    if chunk.ndim != 3 or not 1 <= valid_len <= chunk.shape[2]:
        raise ValueError("valid_len must select at least one feature frame")
    return chunk.astype(np.float32, copy=False)


def attach_end_to_end_timings(events: list[dict[str, Any]], frontend_ms: float, parser_ms: float) -> None:
    if not events:
        return
    frontend_share = frontend_ms / len(events)
    for index, event in enumerate(events):
        event["frontend_ms"] = frontend_share
        event["parser_ms"] = parser_ms if index == len(events) - 1 else 0.0
        event["end_to_end_ms"] = event["inference_ms"] + event["frontend_ms"] + event["parser_ms"]


def wait_for_probe_slot(
    mode: str,
    scheduled_s: float,
    started_at: float,
    sleep_fn: Any = time.sleep,
    now_fn: Any = time.perf_counter,
) -> float | None:
    if mode == "qualification":
        return None
    if mode != "latency":
        raise ValueError(f"unsupported execution mode: {mode}")
    remaining = started_at + scheduled_s - now_fn()
    if remaining > 0:
        sleep_fn(remaining)
    return round(now_fn() - started_at, 6)


def probe_event(
    mode: str,
    scheduled_s: float,
    lease_start_s: float | None,
    feature_frames: list[dict[str, int]],
    encoder_ms: float,
    decoder_ms: float,
) -> dict[str, Any]:
    if mode == "qualification":
        return {"logical_offset_s": scheduled_s, "feature_frames": feature_frames}
    if mode != "latency":
        raise ValueError(f"unsupported execution mode: {mode}")
    return {
        "scheduled_start_s": scheduled_s,
        "enqueue_s": scheduled_s,
        "lease_start_s": lease_start_s,
        "feature_frames": feature_frames,
        "inference_ms": encoder_ms + decoder_ms,
        "encoder_ms": encoder_ms,
        "decoder_ms": decoder_ms,
    }


def require_latency_survivor(
    candidate: str,
    provider: str,
    score: float,
    survivors: set[tuple[str, str, float]],
) -> None:
    if (candidate, provider, score) not in survivors:
        raise RuntimeError(f"{candidate}/{provider}/{score:g} is not a 14/14 qualification survivor")


def qualification_checkpoint_valid(checkpoint: dict[str, Any], audio: dict[str, Any]) -> bool:
    recorded_audio = checkpoint.get("audio") or {}
    return (
        recorded_audio.get("id") == audio.get("id")
        and recorded_audio.get("negative") is False
        and recorded_audio.get("sha256") == audio.get("sha256")
        and isinstance(checkpoint.get("transcript"), str)
        and "parser_result" in checkpoint
        and "eou_t_s" in checkpoint
        and checkpoint.get("finite") is True
        and checkpoint.get("cache_shapes") == EXPECTED_CACHE_SHAPES
        and isinstance(checkpoint.get("probes"), list)
    )


def migrate_qualification_checkpoint(
    checkpoint: dict[str, Any],
    audio: dict[str, Any],
    runner_hash: str,
) -> dict[str, Any]:
    if not qualification_checkpoint_valid(checkpoint, audio):
        raise ValueError("checkpoint is not reusable qualification evidence")
    logical_probes = []
    for probe in checkpoint["probes"]:
        offset = probe.get("logical_offset_s", probe.get("scheduled_start_s"))
        if offset is None:
            raise ValueError("qualification probe lacks logical offset")
        logical_probes.append(
            {
                "logical_offset_s": float(offset),
                "feature_frames": probe.get("feature_frames", []),
            }
        )
    migrated = {
        "schema": QUALIFICATION_SCHEMA,
        "runner_sha256": runner_hash,
        "audio": audio,
        "transcript": checkpoint["transcript"],
        "parser_result": checkpoint["parser_result"],
        "eou_t_s": checkpoint["eou_t_s"],
        "trigger_onset_s": checkpoint.get("trigger_onset_s", checkpoint["eou_t_s"]),
        "probe_decoder": checkpoint.get("probe_decoder"),
        "probes": logical_probes,
        "cache_shapes": checkpoint["cache_shapes"],
        "finite": True,
    }
    previous_hash = checkpoint.get("runner_sha256")
    if previous_hash and previous_hash != runner_hash:
        migrated["migrated_from_runner_sha256"] = previous_hash
    return migrated


def probe_due(frame_end: int, last_probe_frame: int) -> bool:
    return frame_end >= last_probe_frame + 25


class EouRuntime:
    def __init__(self, bundle: Path, provider: str, score: float, parser: ProductionParser):
        self.encoder = ort.InferenceSession(str(bundle / "encoder.onnx"), providers=providers(provider))
        # DML owns cache encoder; tiny recurrent joint stays CPU per probe diagnostic.
        self.decoder = ort.InferenceSession(str(bundle / "decoder_joint.onnx"), providers=["CPUExecutionProvider"])
        if self.encoder.get_providers()[0] != providers(provider)[0] or self.decoder.get_providers()[0] != "CPUExecutionProvider":
            raise RuntimeError(f"provider fallback: {self.encoder.get_providers()} / {self.decoder.get_providers()}")
        self.tokenizer, self.phrases = phrase_ids(next((bundle / "tokenizer").glob("*_tokenizer.model")))
        self.score = score
        self.parser = parser

    def warm(self) -> None:
        channel = np.zeros((1, 17, 70, 512), np.float32); temporal = np.zeros((1, 17, 512, 8), np.float32); cache_len = np.zeros((1,), np.int64)
        for length in (9, 25):
            audio = np.zeros((1, 128, 25), np.float32)
            output = self.encoder.run(None, {"audio_signal": audio, "length": np.asarray([length], np.int64), "cache_last_channel": channel, "cache_last_time": temporal, "cache_last_channel_len": cache_len})
            channel, temporal, cache_len = output[2:]

    def decode(
        self,
        features: np.ndarray,
        duration_s: float,
        frontend_ms: float = 0.0,
        mode: str = "qualification",
    ) -> dict[str, Any]:
        channel = np.zeros((1, 17, 70, 512), np.float32)
        temporal = np.zeros((1, 17, 512, 8), np.float32)
        cache_len = np.zeros((1,), np.int64)
        state1 = np.zeros((1, 1, 640), np.float32)
        state2 = state1.copy()
        last = BLANK
        tokens: list[int] = []; prefix: list[int] = []
        eou_t: float | None = None
        events: list[dict[str, Any]] = []
        skipped = 0
        available = features.shape[2]
        slot_end = 25
        slot_encoder_ms = 0.0
        slot_decoder_ms = 0.0
        slot_steps: list[dict[str, int]] = []
        started_at = time.perf_counter()
        slot_lease_start_s: float | None = None
        for index, (start, pre, new) in enumerate(cache_steps(available)):
            count = pre + new
            chunk = np.zeros((1, features.shape[1], 25), np.float32)
            chunk[:, :, :count] = features[:, :, start : start + count]
            chunk = normalize_encoder_features(chunk, count)
            frame_end = start + pre + new
            this_slot = int(np.ceil(frame_end / 25.0)) * 25
            if this_slot > slot_end:
                events.append(probe_event(mode, round(slot_end / 100.0, 3), slot_lease_start_s, slot_steps, slot_encoder_ms, slot_decoder_ms))
                slot_end = this_slot; slot_encoder_ms = 0.0; slot_decoder_ms = 0.0; slot_steps = []; slot_lease_start_s = None
            scheduled = round(slot_end / 100.0, 3)
            if slot_lease_start_s is None:
                slot_lease_start_s = wait_for_probe_slot(mode, scheduled, started_at)
            started = time.perf_counter()
            output = self.encoder.run(None, {"audio_signal": chunk, "length": np.asarray([count], np.int64), "cache_last_channel": channel, "cache_last_time": temporal, "cache_last_channel_len": cache_len})
            encoder_ms = (time.perf_counter() - started) * 1000
            slot_encoder_ms += encoder_ms
            encoded, lengths, channel, temporal, cache_len = output
            decoder_ms = 0.0
            for frame in range(int(lengths[0])):
                for _ in range(10):
                    dt = time.perf_counter()
                    result = self.decoder.run(None, {"encoder_outputs": encoded[:, :, frame:frame+1], "targets": np.asarray([[last]], np.int32), "target_length": np.asarray([1], np.int32), "input_states_1": state1, "input_states_2": state2})
                    decoder_ms += (time.perf_counter() - dt) * 1000
                    logits = result[0][0, 0, 0].copy()
                    for expected in next_bias(prefix, self.phrases): logits[expected] += self.score
                    token = int(np.argmax(logits))
                    if token == BLANK: break
                    state1, state2 = result[2], result[3]
                    tokens.append(token); last = token
                    if token == EOU:
                        if eou_t is None: eou_t = scheduled
                        prefix.clear(); last = BLANK; state1.fill(0); state2.fill(0)
                    elif token < EOU: prefix.append(token)
            slot_decoder_ms += decoder_ms
            slot_steps.append({"pre": pre, "new": new, "encoded_frames": int(lengths[0])})
        if slot_steps:
            events.append(probe_event(mode, round(slot_end / 100.0, 3), slot_lease_start_s, slot_steps, slot_encoder_ms, slot_decoder_ms))
        parser_started = time.perf_counter()
        transcript = self.tokenizer.decode([token for token in tokens if token < EOU])
        parser_result = self.parser.parse(transcript)
        parser_ms = (time.perf_counter() - parser_started) * 1000
        result = {"transcript": transcript, "parser_result": parser_result, "eou_t_s": eou_t, "trigger_onset_s": eou_t, "probe_decoder": {"mode": "greedy_rnnt", "beam_width": 1, "prefix_bias": self.score, "phrases": list(PHRASES), "eou_id": EOU, "eob_id": EOB, "blank_id": BLANK}, "probes": events, "cache_shapes": [list(channel.shape), list(temporal.shape)], "finite": bool(np.isfinite(channel).all() and np.isfinite(temporal).all())}
        if mode == "latency":
            attach_end_to_end_timings(events, frontend_ms, parser_ms)
            result.update({"frontend_ms": frontend_ms, "parser_ms": parser_ms, "skipped_probes": skipped})
        return result


def raw_centered_logmel(audio: np.ndarray) -> np.ndarray:
    """Bundle frontend: centered, pre-emphasized raw log-mel without CMVN."""
    from onnx_asr.preprocessors.numpy_preprocessor import NemoPreprocessorNumpy
    preprocessor = NemoPreprocessorNumpy("nemo128")
    samples = audio.astype(np.float32, copy=False)[None, :]
    samples = samples - preprocessor._preemph * np.pad(samples, ((0, 0), (1, 0)))[:, :-1]
    samples = np.pad(samples, ((0, 0), (preprocessor._n_fft // 2, preprocessor._n_fft // 2)))
    frames = np.lib.stride_tricks.sliding_window_view(samples, preprocessor._n_fft, axis=1)[:, ::preprocessor._hop_length]
    pad = (preprocessor._n_fft - preprocessor._win_length) // 2
    window = np.pad(np.hanning(preprocessor._win_length), ((pad, pad)))
    spectrum = np.abs(np.fft.rfft(frames * window, preprocessor._n_fft)).astype(np.float32) ** 2
    return np.log(spectrum @ preprocessor._melscale_fbanks + preprocessor._log_zero_guard_value).transpose(0, 2, 1).astype(np.float32)


def features_for(audio_path: Path) -> tuple[np.ndarray, float, float]:
    started = time.perf_counter()
    audio, rate = sf.read(audio_path, dtype="float32", always_2d=False)
    if rate != SR: raise RuntimeError(f"expected 16kHz: {audio_path}")
    if audio.ndim == 2: audio = audio.mean(axis=1)
    features = raw_centered_logmel(audio)
    return features, len(audio) / SR, (time.perf_counter() - started) * 1000


def candidates(receipt: Path) -> list[dict[str, Any]]:
    rows = json.loads(receipt.read_text(encoding="utf-8"))["candidates"]
    if len(rows) != 20 or len({x["encoder"]["sha256"] for x in rows}) != 20: raise RuntimeError("receipt must have 20 unique candidates")
    return rows


def canonical_eval_items(root: Path) -> list[dict[str, Any]]:
    """Return only locally hash-verified canonical-eval negatives."""
    manifest = json.loads((root / "candidate_manifest.json").read_text(encoding="utf-8"))
    rows = manifest["clips"] if isinstance(manifest, dict) and "clips" in manifest else manifest
    if len(rows) != 238:
        raise RuntimeError(f"canonical-eval count mismatch: {len(rows)}")
    items = []
    for row in rows:
        path = root / "candidate_audio" / f"{row['id']}.wav"
        if not path.is_file() or sha256(path).lower() != row["sha256"].lower():
            raise RuntimeError(f"canonical-eval hash mismatch: {row['id']}")
        items.append({"id": row["id"], "path": path, "sha256": row["sha256"].upper(), "negative": True})
    if len({item["id"] for item in items}) != 238:
        raise RuntimeError("canonical-eval IDs must be unique")
    return items


def full_input_items(audio_root: Path, interference: Path, canonical_eval: Path) -> list[dict[str, Any]]:
    take_ids = [1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    controls = [{"id": f"take_{take_id}", "path": audio_root / f"take_{take_id}.wav", "negative": False} for take_id in take_ids]
    return controls + canonical_eval_items(canonical_eval)


def select_input_items(items: list[dict[str, Any]], requested: list[str] | None, all_inputs: bool) -> list[dict[str, Any]]:
    if requested:
        selected = [item for item in items if item["id"] in set(requested)]
        if not selected: raise RuntimeError("no requested frozen input")
        return selected
    return items if all_inputs else items[:14]


def binary_control_verdict(clip: dict[str, Any]) -> str:
    """Binary contract: positives need EOU plus shared-parser control; negatives need none."""
    control_fired = clip.get("eou_t_s") is not None and clip.get("parser_result") is not None
    negative = clip["audio"]["negative"]
    if negative is True: return "pass_negative" if not control_fired else "fail_negative"
    if negative is False: return "pass_positive" if control_fired else "fail_positive"
    return "diagnostic"


def score_runtime_row(row: dict[str, Any]) -> dict[str, Any]:
    verdicts = [binary_control_verdict(clip) for clip in row["clips"]]
    return {
        "candidate": row["candidate"], "provider": row["provider"], "score": row["score"],
        "positive_total": verdicts.count("pass_positive") + verdicts.count("fail_positive"),
        "positive_pass": verdicts.count("pass_positive"),
        "negative_total": verdicts.count("pass_negative") + verdicts.count("fail_negative"),
        "negative_pass": verdicts.count("pass_negative"),
        "false_controls": verdicts.count("fail_negative"),
        "complete": "fail_positive" not in verdicts and "fail_negative" not in verdicts,
    }


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(value, indent=2, sort_keys=True), encoding="utf-8")
    os.replace(temp, path)


def load_survivors(path: Path) -> set[tuple[str, str, float]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    rows = payload.get("rows", payload.get("survivors", []))
    survivors = {
        (str(row["candidate"]), str(row["provider"]), float(row["score"]))
        for row in rows
        if row.get("positive_passes") == 14 and row.get("gate_clean") is True
    }
    if not survivors:
        raise RuntimeError("latency survivor manifest contains no 14/14 qualification survivors")
    return survivors


def preflight(args: argparse.Namespace) -> int:
    rows = candidates(args.receipt)
    for row in rows:
        if row["runtime_gates"]["CPUExecutionProvider"]["status"] != "ok" or row["runtime_gates"]["DmlExecutionProvider"]["status"] != "ok":
            raise RuntimeError(f"runtime gate invalid: {row['name']}")
    tokenizer = Path(rows[0]["tokenizer_assets"][1]["path"])
    processor, ids = phrase_ids(tokenizer)
    expected = ((409,345,1005,1011,1004,73,112), (409,345,1005,1011,1004,92,13), (409,345,1005,1011,1004,478,855), (409,345,1005,1011,1004,110,84,1006))
    if ids != expected or processor.piece_to_id("<EOU>") != EOU or processor.piece_to_id("<EOB>") != EOB or processor.get_piece_size() != 1026:
        raise RuntimeError("locked SentencePiece contract mismatch")
    inputs = [args.audio_root / f"take_{x}.wav" for x in [1,2,3,4,5,7,8,9,10,11,12,13,14,15]]
    if not all(path.is_file() for path in inputs) or sha256(args.interference) != "AD55DDCE9C61C47071603EAB34A7D75569151C9F5C5F6C3D3F5332D5946183A7":
        raise RuntimeError("frozen corpus input mismatch")
    manifest = [{"id": path.stem, "path": str(path), "sha256": sha256(path)} for path in inputs]
    negatives = canonical_eval_items(args.canonical_eval)
    atomic_json(args.out / "corpus-manifest.json", {"takes": manifest, "interference": {"path": str(args.interference), "sha256": sha256(args.interference)}, "canonical_eval_negatives": [{"id": item["id"], "path": str(item["path"]), "sha256": item["sha256"]} for item in negatives]})
    atomic_json(args.out / "decoder-contract.json", {"vocab_size": 1026, "eou": EOU, "eob": EOB, "blank": BLANK, "beam_width": 1, "scores": [0.0, PREFIX_BIAS], "phrase_ids": dict(zip(PHRASES, ids))})
    atomic_json(args.out / "checkpoint-index.json", {"schema": "heardright.windows-eou-checkpoint-index.v1", "receipt_sha256": sha256(args.receipt), "candidate_count": 20, "complete": []})
    (args.out.parent / "RUN_PACKET.md").write_text("# Windows SentencePiece EOU bakeoff\n\nFrozen by `windows_realtime_eou_bakeoff.py --preflight`.\n", encoding="utf-8")
    print(f"preflight ok candidates={len(rows)} takes={len(manifest)} negatives={len(negatives)} ids={ids}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", type=Path, required=True); parser.add_argument("--audio-root", type=Path, required=True)
    parser.add_argument("--interference", type=Path, required=True); parser.add_argument("--mel", type=Path, required=True)
    parser.add_argument("--canonical-eval", type=Path, default=Path(__file__).parent / "open-asr-bakeoff" / "canonical-eval")
    parser.add_argument("--parser-helper", type=Path, default=Path(__file__).parent.parent / "heardright-engine" / "target" / "debug" / "examples" / "eou_control_parser.exe")
    parser.add_argument("--out", type=Path, required=True); parser.add_argument("--candidate", action="append")
    parser.add_argument("--provider", choices=("cpu", "dml"), action="append")
    parser.add_argument("--input", action="append", help="frozen take_N or canonical negative id")
    parser.add_argument("--score", type=float, action="append", choices=(0.0, PREFIX_BIAS))
    parser.add_argument("--mode", choices=("qualification", "latency"), default="qualification")
    parser.add_argument("--survivors", type=Path)
    parser.add_argument("--all", action="store_true"); parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--preflight", action="store_true"); parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args(); args.out.mkdir(parents=True, exist_ok=True)
    if args.preflight: return preflight(args)
    retained = full_input_items(args.audio_root, args.interference, args.canonical_eval)
    inputs = select_input_items(retained, args.input, args.all)
    if any(item["negative"] for item in inputs):
        raise RuntimeError(f"{args.mode} mode accepts retained positive controls only")
    if args.mode == "latency" and args.survivors is None:
        raise RuntimeError("latency mode requires --survivors qualification proof")
    survivors = load_survivors(args.survivors) if args.mode == "latency" else set()
    raw = []
    for source in inputs:
        path = source["path"]
        feats, duration, frontend_ms = features_for(path); np.save(args.out / f"{path.stem}.npy", feats, allow_pickle=False)
        item = {"id": source["id"], "negative": source["negative"], "path": str(path), "sha256": sha256(path), "duration_s": duration, "feature_file": f"{path.stem}.npy", "feature_sha256": sha256(args.out / f"{path.stem}.npy")}
        if args.mode == "latency":
            item["frontend_ms"] = frontend_ms
        raw.append(item)
    rows = candidates(args.receipt)
    if args.candidate: rows = [x for x in rows if x["name"] in set(args.candidate)]
    results = []
    runner_hash = sha256(Path(__file__))
    for row in rows:
        bundle = Path(row["encoder"]["path"]).parent
        for provider in (args.provider or ["cpu", "dml"]):
          for score in (args.score or [0.0, PREFIX_BIAS]):
            if args.mode == "latency":
                require_latency_survivor(row["name"], provider, score, survivors)
            with ProductionParser(args.parser_helper) as production_parser:
                before = rss_bytes(); runtime = EouRuntime(bundle, provider, score, production_parser); runtime.warm(); loaded = rss_bytes()
                clips = []
                for item in raw:
                    score_key = str(score).replace(".", "_")
                    checkpoint_root = "checkpoints" if args.mode == "qualification" else "latency-checkpoints"
                    checkpoint = args.out / checkpoint_root / row["name"] / provider / score_key / f"{Path(item['path']).stem}.json"
                    existing = json.loads(checkpoint.read_text(encoding="utf-8")) if checkpoint.is_file() else None
                    if args.mode == "qualification" and existing and qualification_checkpoint_valid(existing, item):
                        clip = migrate_qualification_checkpoint(existing, item, runner_hash)
                        if clip != existing:
                            atomic_json(checkpoint, clip)
                    elif args.mode == "latency" and existing and existing.get("runner_sha256") == runner_hash:
                        clip = existing
                    else:
                        feats = np.load(args.out / item["feature_file"], allow_pickle=False)
                        clip = {
                            "schema": QUALIFICATION_SCHEMA if args.mode == "qualification" else "heardright.windows-eou-latency-checkpoint.v1",
                            "runner_sha256": runner_hash,
                            "audio": item,
                            **runtime.decode(feats, float(item["duration_s"]), float(item.get("frontend_ms", 0.0)), args.mode),
                        }
                        atomic_json(checkpoint, clip)
                    clips.append(clip)
                runtime_result = {
                    "candidate": row["name"],
                    "encoder_sha256": row["encoder"]["sha256"],
                    "provider": provider,
                    "score": score,
                    "runtime_valid": True,
                    "encoder_providers": runtime.encoder.get_providers(),
                    "decoder_providers": runtime.decoder.get_providers(),
                    "clips": clips,
                }
                if args.mode == "latency":
                    runtime_result["runtime_residency_rss_bytes"] = None if before is None or loaded is None else loaded-before
                results.append(runtime_result)
                del runtime
    payload = {
        "schema": f"heardright.windows-eou-{args.mode}.v1",
        "mode": args.mode,
        "binary_trigger_contract": "positive=EOU+shared-parser-control",
        "raw_audio": raw,
        "results": results,
    }
    if args.mode == "latency":
        payload.update({"cadence_s": CADENCE_S, "duty_governor": False})
    raw_path = args.out / f"{args.mode}-raw.json"
    summary_path = args.out / f"{args.mode}-summary.json"
    atomic_json(raw_path, payload)
    atomic_json(summary_path, {"schema": f"heardright.windows-eou-{args.mode}-matrix.v1", "rows": [score_runtime_row(row) for row in results]})
    print(f"{args.mode} completed candidates={len(rows)} runtime_rows={len(results)} out={raw_path}")
    return 0


if __name__ == "__main__": raise SystemExit(main())
