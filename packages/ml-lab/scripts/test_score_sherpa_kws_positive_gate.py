import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("score_sherpa_kws_positive_gate.py")
CANONICAL_IDS = (
    "take_1", "take_2", "take_3", "take_4", "take_5", "take_7", "take_8",
    "take_9", "take_10", "take_11", "take_12", "take_13", "take_14", "take_15",
)


def load_module():
    spec = importlib.util.spec_from_file_location(
        "score_sherpa_kws_positive_gate", MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SherpaPositiveGateScorerTests(unittest.TestCase):
    def test_requires_per_clip_expected_detection(self):
        module = load_module()

        with self.assertRaisesRegex(ValueError, "expected_detection"):
            module.score_rows(
                [{"id": "take_1", "sha256": "A" * 64}],
                [{"row_id": "take_1", "audio_sha256": "A" * 64, "detections": ["ZEPHYR_STOP"]}],
            )

    def test_rejects_unsupported_intent(self):
        module = load_module()

        with self.assertRaisesRegex(ValueError, "allowed intent"):
            module.score_rows(
                [{"id": "take_1", "sha256": "A" * 64, "expected_detection": "ZEPHYR_UNKNOWN"}],
                [{"row_id": "take_1", "audio_sha256": "A" * 64, "detections": ["ZEPHYR_UNKNOWN"]}],
            )

    def test_accepts_distinct_expected_control_labels(self):
        module = load_module()
        authority = [
            {"id": "take_1", "sha256": "A" * 64, "expected_detection": "ZEPHYR_STOP"},
            {"id": "take_4", "sha256": "B" * 64, "expected_detection": "ZEPHYR_SEND"},
            {"id": "take_9", "sha256": "C" * 64, "expected_detection": "ZEPHYR_CANCEL"},
        ]
        rows = [
            {"row_id": item["id"], "audio_sha256": item["sha256"], "detections": [item["expected_detection"]]}
            for item in authority
        ]

        report = module.score_rows(authority, rows)

        self.assertTrue(report["passed"])
        self.assertEqual(report["recognized"], 3)

    def test_expands_qualification_raw_audio_into_exact_intents(self):
        module = load_module()
        raw_audio = [
            {"id": clip_id, "path": f"C:/clips/{clip_id}.wav", "sha256": f"{index:064X}"}
            for index, clip_id in enumerate(CANONICAL_IDS, start=1)
        ]
        clips = [
            {"audio": {"id": row["id"], "sha256": row["sha256"]}, "parser_result": {"intent": "Stop" if row["id"] != "take_2" else "Send"}}
            for row in raw_audio
        ]

        authority = module.authority_from_manifest({"raw_audio": raw_audio, "results": [{"clips": clips}]})

        self.assertEqual(len(authority), 14)
        self.assertEqual(authority[0]["id"], "take_1")
        self.assertEqual(authority[1]["expected_detection"], "ZEPHYR_SEND")

    def test_rejects_duplicate_raw_audio_id(self):
        module = load_module()
        raw_audio = [
            {"id": clip_id, "path": f"C:/clips/{clip_id}.wav", "sha256": f"{index:064X}"}
            for index, clip_id in enumerate(CANONICAL_IDS, start=1)
        ]
        clips = [
            {"audio": {"id": row["id"], "sha256": row["sha256"]}, "parser_result": {"intent": "Stop"}}
            for row in raw_audio
        ]
        raw_audio.append(dict(raw_audio[0]))

        with self.assertRaisesRegex(ValueError, "exactly 14"):
            module.authority_from_manifest({"raw_audio": raw_audio, "results": [{"clips": clips}]})

    def test_rejects_extra_raw_audio_row(self):
        module = load_module()
        raw_audio = [
            {"id": clip_id, "path": f"C:/clips/{clip_id}.wav", "sha256": f"{index:064X}"}
            for index, clip_id in enumerate(CANONICAL_IDS, start=1)
        ]
        clips = [
            {"audio": {"id": row["id"], "sha256": row["sha256"]}, "parser_result": {"intent": "Stop"}}
            for row in raw_audio
        ]
        raw_audio.append({"id": "extra", "path": "C:/clips/extra.wav", "sha256": "E" * 64})

        with self.assertRaisesRegex(ValueError, "exactly 14"):
            module.authority_from_manifest({"raw_audio": raw_audio, "results": [{"clips": clips}]})

    def test_rejects_legacy_takes_only_manifest(self):
        module = load_module()

        with self.assertRaisesRegex(ValueError, "raw_audio"):
            module.authority_from_manifest({
                "takes": [{"id": "take_1", "path": "C:/clips/take_1.wav", "sha256": "A" * 64}]
            })

    def test_rejects_rogue_raw_result_row(self):
        module = load_module()
        authority = [
            {"id": "take_1", "sha256": "A" * 64, "expected_detection": "ZEPHYR_STOP"}
        ]
        rows = [
            {"row_id": "take_1", "audio_sha256": "A" * 64, "detections": ["ZEPHYR_STOP"]},
            {"row_id": "rogue", "audio_sha256": "B" * 64, "detections": ["ZEPHYR_STOP"]},
        ]

        with self.assertRaisesRegex(ValueError, "exactly match"):
            module.score_rows(authority, rows)

    def test_rejects_duplicate_raw_result_row(self):
        module = load_module()
        authority = [
            {"id": "take_1", "sha256": "A" * 64, "expected_detection": "ZEPHYR_STOP"}
        ]
        rows = [
            {"row_id": "take_1", "audio_sha256": "A" * 64, "detections": ["ZEPHYR_STOP"]},
            {"row_id": "take_1-repeat", "audio_sha256": "A" * 64, "detections": ["ZEPHYR_STOP"]},
        ]

        with self.assertRaisesRegex(ValueError, "exactly match"):
            module.score_rows(authority, rows)

    def test_uses_parser_result_intent_for_authority(self):
        module = load_module()
        raw_audio = [
            {"id": clip_id, "path": f"C:/clips/{clip_id}.wav", "sha256": f"{index:064X}"}
            for index, clip_id in enumerate(CANONICAL_IDS, start=1)
        ]
        clips = [
            {"audio": {"id": row["id"], "sha256": row["sha256"]}, "parser_result": {"intent": "Stop"}}
            for row in raw_audio
        ]
        clips[0]["parser_result"]["intent"] = "Send"

        authority = module.authority_from_manifest({"raw_audio": raw_audio, "results": [{"clips": clips}]})

        self.assertEqual(authority[0]["id"], "take_1")
        self.assertEqual(authority[0]["expected_detection"], "ZEPHYR_SEND")

    def test_accepts_locked_four_positive_smoke_count(self):
        module = load_module()
        authority = [{"id": f"take_{index}", "sha256": f"{index:064X}"} for index in range(4)]

        module.assert_authority_count(authority, 4)

    def test_all_exact_stop_detections_pass(self):
        module = load_module()
        authority = [
            {
                "id": f"take_{index}",
                "sha256": f"{index:064X}",
                "expected_detection": "ZEPHYR_STOP",
            }
            for index in range(1, 15)
        ]
        rows = [
            {"row_id": item["id"], "audio_sha256": item["sha256"], "detections": ["ZEPHYR_STOP"]}
            for item in authority
        ]

        report = module.score_rows(authority, rows)

        self.assertTrue(report["passed"])
        self.assertEqual(report["recognized"], 14)
        self.assertEqual(report["total"], 14)

    def test_miss_and_wrong_command_fail(self):
        module = load_module()
        authority = [
            {"id": "take_1", "sha256": "A" * 64, "expected_detection": "ZEPHYR_STOP"},
            {"id": "take_2", "sha256": "B" * 64, "expected_detection": "ZEPHYR_STOP"},
        ]
        rows = [
            {"row_id": "take_1", "audio_sha256": "A" * 64, "detections": []},
            {"row_id": "take_2", "audio_sha256": "B" * 64, "detections": ["ZEPHYR_SEND"]},
        ]

        report = module.score_rows(authority, rows)

        self.assertFalse(report["passed"])
        self.assertEqual(report["recognized"], 0)
        self.assertEqual([item["status"] for item in report["rows"]], ["miss", "wrong"])


if __name__ == "__main__":
    unittest.main()
