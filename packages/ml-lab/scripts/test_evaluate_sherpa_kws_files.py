import hashlib
import importlib.util
import json
import tempfile
import unittest
import wave
from pathlib import Path
from types import SimpleNamespace

import numpy as np


MODULE_PATH = Path(__file__).with_name("evaluate_sherpa_kws_files.py")


def load_module():
    spec = importlib.util.spec_from_file_location("evaluate_sherpa_kws_files", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SherpaKwsFileRunnerTests(unittest.TestCase):
    def test_fresh_windows_overlap_and_cover_trailing_audio(self):
        module = load_module()

        self.assertEqual(
            module.fresh_window_spans(4_100, window_samples=1_500, hop_samples=250),
            [
                (0, 1_500), (250, 1_750), (500, 2_000), (750, 2_250),
                (1_000, 2_500), (1_250, 2_750), (1_500, 3_000),
                (1_750, 3_250), (2_000, 3_500), (2_250, 3_750),
                (2_500, 4_000), (2_600, 4_100),
            ],
        )
        self.assertEqual(
            module.fresh_window_spans(800, window_samples=1_500, hop_samples=250),
            [(0, 800)],
        )

    def test_overlapping_wake_hits_are_deduplicated_by_absolute_samples(self):
        module = load_module()
        candidates = [
            {"keyword": "ZEPHYR", "start_sample": 10_000, "end_sample": 17_000},
            {"keyword": "ZEPHYR", "start_sample": 10_240, "end_sample": 17_360},
            {"keyword": "ZEPHYR", "start_sample": 40_000, "end_sample": 47_000},
        ]

        self.assertEqual(
            module.deduplicate_candidates(candidates, refractory_samples=8_000),
            [candidates[0], candidates[2]],
        )

    def test_confirmation_endpoints_accrue_forward_until_timeout(self):
        module = load_module()

        self.assertEqual(
            module.accruing_confirmation_ends(
                candidate_end_sample=20_000,
                available_end_sample=42_300,
                step_samples=3_200,
                timeout_samples=19_200,
            ),
            [23_200, 26_400, 29_600, 32_800, 36_000, 39_200],
        )
        self.assertEqual(
            module.accruing_confirmation_ends(
                candidate_end_sample=20_000,
                available_end_sample=25_000,
                step_samples=3_200,
                timeout_samples=19_200,
            ),
            [23_200, 25_000],
        )

    def test_chunk_lifecycle_stops_on_first_hit_without_input_finished(self):
        module = load_module()

        class Result:
            keyword = "ZEPHYR_SEND"
            tokens = ["Z", "EH1"]
            timestamps = [1.0, 1.04]
            start_time = 0.0
            json = '{"keyword":"ZEPHYR_SEND"}'

        class Stream:
            def __init__(self):
                self.accepted = []
                self.finished = False

            def accept_waveform(self, sample_rate, samples):
                self.accepted.append((sample_rate, len(samples)))

            def input_finished(self):
                self.finished = True

        class NativeSpotter:
            def __init__(self):
                self.get_result_calls = 0

            def get_result(self, _stream):
                self.get_result_calls += 1
                return Result()

        class Spotter:
            def __init__(self):
                self.keyword_spotter = NativeSpotter()
                self.decode_calls = 0

            def create_stream(self):
                return Stream()

            def is_ready(self, stream):
                return bool(stream.accepted) and self.decode_calls == 0

            def decode_stream(self, _stream):
                self.decode_calls += 1

        spotter = Spotter()
        actual = module.decode_samples_until_first_hit(
            spotter, np.ones(2_560, dtype=np.float32), 16_000, chunk_ms=80, tail_ms=1_000
        )

        self.assertEqual(spotter.decode_calls, 1)
        self.assertEqual(spotter.keyword_spotter.get_result_calls, 1)
        self.assertFalse(actual["input_finished"])
        self.assertEqual(actual["detected_at_sample"], 1_280)
        self.assertEqual(actual["stream_accept_sizes"], [1_280])
        self.assertEqual(actual["result"]["keyword"], "ZEPHYR_SEND")
        self.assertEqual(len(actual["decode_step_ms"]), 1)

    def test_chunk_lifecycle_feeds_tail_then_finishes_and_drains(self):
        module = load_module()

        class Result:
            keyword = "ZEPHYR_CANCEL"
            tokens = ["Z"]
            timestamps = [0.5]
            start_time = 0.0
            json = "{}"

        class Stream:
            def __init__(self):
                self.accepted = []
                self.finished = False

            def accept_waveform(self, _sample_rate, samples):
                self.accepted.append(len(samples))

            def input_finished(self):
                self.finished = True

        class NativeSpotter:
            def get_result(self, _stream):
                return Result()

        class Spotter:
            def __init__(self):
                self.keyword_spotter = NativeSpotter()
                self.decode_calls = 0
                self.stream = Stream()

            def create_stream(self):
                return self.stream

            def is_ready(self, stream):
                return stream.finished and self.decode_calls == 0

            def decode_stream(self, _stream):
                self.decode_calls += 1

        spotter = Spotter()
        actual = module.decode_samples_until_first_hit(
            spotter, np.ones(1_280, dtype=np.float32), 16_000, chunk_ms=80, tail_ms=160
        )

        self.assertTrue(actual["input_finished"])
        self.assertEqual(spotter.stream.accepted, [1_280, 1_280, 1_280])
        self.assertEqual(actual["detected_at_sample"], 3_840)
        self.assertEqual(actual["result"]["keyword"], "ZEPHYR_CANCEL")

    def test_manifest_binds_all_canonical_allowed_intents(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "qualification-raw.json"
            rows = []
            clips = []
            for index, clip_id in enumerate(module.CANONICAL_CLIP_IDS, start=1):
                path = root / f"{clip_id}.wav"
                path.write_bytes(bytes([index]))
                rows.append({"id": clip_id, "path": str(path), "sha256": module.sha256_file(path)})
                clips.append({"audio": {"id": clip_id, "sha256": rows[-1]["sha256"]}, "parser_result": {"intent": ("Stop", "Send", "Cancel")[(index - 1) % 3]}})
            manifest.write_text(json.dumps({"raw_audio": rows, "results": [{"clips": clips}]}), encoding="utf-8")

            loaded = module.load_exact_intent_manifest(manifest)

        self.assertEqual(len(loaded), 14)
        self.assertEqual({row["expected_detection"] for row in loaded}, {
            "ZEPHYR_STOP", "ZEPHYR_SEND", "ZEPHYR_CANCEL"
        })
        self.assertTrue(all(row["expected_detection"] in module.ALLOWED_INTENTS for row in loaded))
        self.assertEqual(loaded[0]["id"], "take_1")

    def test_manifest_rejects_duplicate_canonical_id(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = []
            for index, clip_id in enumerate(module.CANONICAL_CLIP_IDS, start=1):
                path = root / f"{clip_id}.wav"
                path.write_bytes(bytes([index]))
                rows.append({"id": clip_id, "path": str(path), "sha256": module.sha256_file(path)})
            rows.append(dict(rows[0]))
            manifest = root / "qualification-raw.json"
            manifest.write_text(json.dumps({"raw_audio": rows}), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "exactly 14"):
                module.load_exact_intent_manifest(manifest)

    def test_manifest_rejects_extra_row(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = []
            for index, clip_id in enumerate(module.CANONICAL_CLIP_IDS, start=1):
                path = root / f"{clip_id}.wav"
                path.write_bytes(bytes([index]))
                rows.append({"id": clip_id, "path": str(path), "sha256": module.sha256_file(path)})
            extra = root / "extra.wav"
            extra.write_bytes(b"extra")
            rows.append({"id": "extra", "path": str(extra), "sha256": module.sha256_file(extra)})
            manifest = root / "qualification-raw.json"
            manifest.write_text(json.dumps({"raw_audio": rows}), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "exactly 14"):
                module.load_exact_intent_manifest(manifest)

    def test_manifest_derives_expected_detection_from_parser_result(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = []
            clips = []
            for index, clip_id in enumerate(module.CANONICAL_CLIP_IDS, start=1):
                path = root / f"{clip_id}.wav"
                path.write_bytes(bytes([index]))
                row = {"id": clip_id, "path": str(path), "sha256": module.sha256_file(path)}
                rows.append(row)
                clips.append({"audio": {"id": clip_id, "sha256": row["sha256"]}, "parser_result": {"intent": "Stop"}})
            clips[0]["parser_result"]["intent"] = "Send"
            manifest = root / "qualification-raw.json"
            manifest.write_text(json.dumps({"raw_audio": rows, "results": [{"clips": clips}]}), encoding="utf-8")

            loaded = module.load_exact_intent_manifest(manifest)

        self.assertEqual(loaded[0]["id"], "take_1")
        self.assertEqual(loaded[0]["expected_detection"], "ZEPHYR_SEND")

    def test_generated_manifest_data_excludes_retired_control(self):
        module = load_module()
        source = (
            Path(__file__).resolve().parents[2]
            / "docs"
            / "benchmarks"
            / "artifacts"
            / "2026-07-28-windows-probe-model-bakeoff"
            / "run"
            / "qualification-raw.json"
        )
        generated = module.load_exact_intent_manifest(source)
        retired_control = "ZEPHYR_" + "SUB" + "MIT"

        self.assertNotIn(retired_control, json.dumps(generated, default=str))
        self.assertTrue(all(row["expected_detection"] in module.ALLOWED_INTENTS for row in generated))

    def test_loads_mixed_owner_regression_manifest(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            positive = root / "positive.wav"
            negative = root / "negative.wav"
            positive.write_bytes(b"positive")
            negative.write_bytes(b"negative")
            manifest = root / "owner.json"
            manifest.write_text(json.dumps({
                "schema": "heardright.owner_kws_regression.v1",
                "rows": [
                    {"id": "owner-positive", "path": str(positive), "sha256": module.sha256_file(positive), "classification": "positive", "expected_detection": "ZEPHYR_SEND"},
                    {"id": "owner-negative", "path": str(negative), "sha256": module.sha256_file(negative), "classification": "negative", "expected_detection": None},
                ],
            }), encoding="utf-8")

            loaded = module.load_labeled_manifest(manifest)

        self.assertEqual([row["expected_detection"] for row in loaded], ["ZEPHYR_SEND", None])

    def test_owner_manifest_rejects_unclassified_or_duplicate_audio(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio = root / "audio.wav"
            audio.write_bytes(b"audio")
            row = {"id": "one", "path": str(audio), "sha256": module.sha256_file(audio), "expected_detection": None}
            manifest = root / "owner.json"
            manifest.write_text(json.dumps({"schema": "heardright.owner_kws_regression.v1", "rows": [row]}), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "classification"):
                module.load_labeled_manifest(manifest)

            duplicate = dict(row, id="two", classification="negative")
            row["classification"] = "negative"
            manifest.write_text(json.dumps({"schema": "heardright.owner_kws_regression.v1", "rows": [row, duplicate]}), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate.*SHA-256"):
                module.load_labeled_manifest(manifest)

    def test_owner_manifest_verifies_audio_hash(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio = root / "audio.wav"
            audio.write_bytes(b"audio")
            manifest = root / "owner.json"
            manifest.write_text(json.dumps({
                "schema": "heardright.owner_kws_regression.v1",
                "rows": [{"id": "one", "path": str(audio), "sha256": "0" * 64, "classification": "negative", "expected_detection": None}],
            }), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "WAV SHA-256"):
                module.load_labeled_manifest(manifest)

    def test_negative_candidate_manifest_is_labeled_negative(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio = root / "negative.wav"
            audio.write_bytes(b"negative")
            manifest = root / "candidate_manifest.json"
            manifest.write_text(json.dumps([{
                "id": "negative-one",
                "path": str(audio),
                "sha256": module.sha256_file(audio),
            }]), encoding="utf-8")

            loaded = module.load_manifest(manifest)

        self.assertEqual(loaded[0]["expected_detection"], None)
        self.assertEqual(loaded[0]["id"], "negative-one")

    def test_config_fingerprint_binds_lifecycle_policy(self):
        module = load_module()
        base = {
            "schema": "heardright.sherpa_kws_file_run.v2",
            "feed_lifecycle": {"chunk_ms": 80, "tail_ms": 1_000, "first_hit": True},
        }
        changed = {
            "schema": "heardright.sherpa_kws_file_run.v2",
            "feed_lifecycle": {"chunk_ms": 80, "tail_ms": 2_000, "first_hit": True},
        }
        self.assertNotEqual(module.config_fingerprint(base), module.config_fingerprint(changed))

    def test_vad_contract_uses_locked_threshold_silence_and_preroll(self):
        module = load_module()

        class Silero:
            pass

        class Config:
            def __init__(self):
                self.silero_vad = Silero()

        class FakeSherpa:
            VadModelConfig = Config

        config = module.build_vad_config(FakeSherpa, Path("silero.onnx"))
        base, segment = module.vad_segment_stream_audio(
            np.arange(10_000, dtype=np.float32), vad_start_sample=5_000, segment_samples=2_000
        )

        self.assertEqual(config.silero_vad.threshold, 0.20)
        self.assertEqual(config.silero_vad.min_silence_duration, 0.25)
        self.assertEqual(config.silero_vad.min_speech_duration, 0.10)
        self.assertEqual(base, 1_000)
        self.assertEqual(len(segment), 6_000)
        wider_base, wider_segment = module.vad_segment_stream_audio(
            np.arange(20_000, dtype=np.float32),
            vad_start_sample=16_000,
            segment_samples=2_000,
            preroll_samples=16_000,
        )
        self.assertEqual(wider_base, 0)
        self.assertEqual(len(wider_segment), 18_000)

    def test_vad_reset_uses_fresh_stream_for_each_segment_and_keeps_first_hit(self):
        module = load_module()

        class Result:
            tokens = ["Z"]
            timestamps = [0.4]
            start_time = 0.0
            json = "{}"

            def __init__(self, keyword):
                self.keyword = keyword

        class Stream:
            def __init__(self, stream_id):
                self.stream_id = stream_id
                self.accepted = 0
                self.decoded = False
                self.finished = False

            def accept_waveform(self, _sample_rate, _samples):
                self.accepted += 1

            def input_finished(self):
                self.finished = True

        class NativeSpotter:
            def get_result(self, stream):
                return Result("ZEPHYR_CANCEL" if stream.stream_id == 2 else "")

        class Spotter:
            def __init__(self):
                self.keyword_spotter = NativeSpotter()
                self.streams = []

            def create_stream(self):
                stream = Stream(len(self.streams) + 1)
                self.streams.append(stream)
                return stream

            def is_ready(self, stream):
                return stream.accepted > 0 and not stream.decoded

            def decode_stream(self, stream):
                stream.decoded = True

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(16_000)
                wav.writeframes(b"\0" * 20_000)
            original_segments = module.speech_segments
            module.speech_segments = lambda *_args: [
                (2_000, np.ones(1_280, dtype=np.float32)),
                (6_000, np.ones(1_280, dtype=np.float32)),
            ]
            try:
                spotter = Spotter()
                actual = module.decode_one_vad_reset(
                    spotter,
                    object(),
                    path,
                    SimpleNamespace(chunk_ms=80, tail_ms=0, vad_model=Path("silero.onnx")),
                )
            finally:
                module.speech_segments = original_segments

        self.assertEqual(len(spotter.streams), 2)
        self.assertIsNot(spotter.streams[0], spotter.streams[1])
        self.assertEqual(actual["keyword"], "ZEPHYR_CANCEL")
        self.assertEqual(actual["vad_start_sample"], 6_000)
        self.assertEqual(actual["segment_base_sample"], 2_000)
        self.assertEqual(actual["detected_at_sample"], 3_280)
    def test_snapshots_keyword_result_once(self):
        module = load_module()

        class Result:
            keyword = "ZEPHYR_SEND"
            tokens = ["Z", "EH1"]
            timestamps = [1.0, 1.04]

        class NativeSpotter:
            def __init__(self):
                self.calls = 0

            def get_result(self, _stream):
                self.calls += 1
                return Result()

        class Spotter:
            def __init__(self):
                self.keyword_spotter = NativeSpotter()

        spotter = Spotter()
        actual = module.snapshot_keyword_result(spotter, object())

        self.assertEqual(spotter.keyword_spotter.calls, 1)
        self.assertEqual(actual["keyword"], "ZEPHYR_SEND")
        self.assertEqual(actual["tokens"], ["Z", "EH1"])

    def test_reads_pcm16_mono_wave_as_float32(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(16000)
                wav.writeframes((-32768).to_bytes(2, "little", signed=True))
                wav.writeframes((16384).to_bytes(2, "little", signed=True))

            samples, sample_rate = module.read_pcm16_mono(path)

        self.assertEqual(sample_rate, 16000)
        self.assertAlmostEqual(float(samples[0]), -1.0)
        self.assertAlmostEqual(float(samples[1]), 0.5)

    def test_rejects_non_mono_wave(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stereo.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(2)
                wav.setsampwidth(2)
                wav.setframerate(16000)
                wav.writeframes(b"\0" * 8)

            with self.assertRaisesRegex(ValueError, "mono"):
                module.read_pcm16_mono(path)

    def test_file_identity_is_sha256(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bytes.bin"
            path.write_bytes(b"heardright")
            self.assertEqual(
                module.sha256_file(path),
                hashlib.sha256(b"heardright").hexdigest().upper(),
            )

    def test_nonempty_detection_is_normalized(self):
        module = load_module()
        self.assertEqual(module.normalize_detection("  ZEPHYR_STOP "), "ZEPHYR_STOP")
        self.assertEqual(module.normalize_detection(""), "")


if __name__ == "__main__":
    unittest.main()
