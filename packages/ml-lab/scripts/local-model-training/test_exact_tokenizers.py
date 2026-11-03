import importlib.util
import json
import unittest
from collections import UserDict
from collections.abc import Mapping
from pathlib import Path


ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("preflight_exact_tokenizers", ROOT / "preflight_exact_tokenizers.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class FakeBackend:
    vocab_size = 100
    chat_template = "{{ messages }}"

    def __call__(self, _text, add_special_tokens=True):
        return BatchEncodingLike({"input_ids": [1, 2, 3]})


class FakeGeneric:
    vocab_size = 100
    chat_template = None

    def __call__(self, _text, add_special_tokens=True):
        return UserDict({"input_ids": (4, 5)})


class BatchEncodingLike(Mapping):
    def __init__(self, values):
        self.values = values

    def __getitem__(self, key):
        return self.values[key]

    def __iter__(self):
        return iter(self.values)

    def __len__(self):
        return len(self.values)


class FakeAutoTokenizer:
    calls = []
    backend_ids = set()

    @classmethod
    def from_pretrained(cls, model_id, **kwargs):
        cls.calls.append((model_id, kwargs))
        return FakeBackend() if model_id in cls.backend_ids else FakeGeneric()


class ExactTokenizerTests(unittest.TestCase):
    def setUp(self):
        self.recipes = json.loads((ROOT / "model-recipes.json").read_text(encoding="utf-8"))
        FakeAutoTokenizer.calls = []
        roster = MODULE.candidate_roster(self.recipes)
        FakeAutoTokenizer.backend_ids = {item["id"] for item in roster if item["family"] in {"falcon", "sotto"}}

    def test_roster_is_six_exact_candidate_revisions(self):
        roster = MODULE.candidate_roster(self.recipes)
        self.assertEqual([item["lane"] for item in roster], ["falcon", "granite", "gemma", "sotto", "falcon_1_5b", "falcon_1_5b_deep"])
        self.assertEqual(len({(item["id"], item["revision"]) for item in roster}), 6)

    def test_probe_loads_and_encodes_all_six_with_exact_revisions(self):
        results = MODULE.probe_candidates(self.recipes, "5.5.0", FakeAutoTokenizer, FakeBackend)
        self.assertEqual(len(results), 6)
        self.assertEqual([call[1]["revision"] for call in FakeAutoTokenizer.calls], [item["revision"] for item in MODULE.candidate_roster(self.recipes)])
        self.assertTrue(all(call[1]["trust_remote_code"] is False for call in FakeAutoTokenizer.calls))
        self.assertTrue(all(result["integer_token_sequence"] for result in results))
        self.assertTrue(all(result["input_ids_sha256"].startswith("sha256:") for result in results))
        self.assertEqual({result["encoded_container_class"] for result in results}, {"BatchEncodingLike", "UserDict"})

    def test_accepts_real_and_batchencoding_like_mappings(self):
        self.assertEqual(tuple(MODULE.validate_encoded_input_ids(UserDict({"input_ids": [1, 2]}), "real-mapping")), (1, 2))
        self.assertEqual(tuple(MODULE.validate_encoded_input_ids(BatchEncodingLike({"input_ids": (3, 4)}), "batch-encoding")), (3, 4))

    def test_rejects_missing_empty_and_non_integer_input_ids(self):
        invalid = [
            (BatchEncodingLike({}), "nonempty input_ids"),
            (BatchEncodingLike({"input_ids": []}), "nonempty input_ids"),
            (BatchEncodingLike({"input_ids": [1, "2"]}), "integer token sequence"),
            (BatchEncodingLike({"input_ids": [True, 2]}), "integer token sequence"),
            ({"input_ids": 7}, "nonempty input_ids"),
        ]
        for encoded, message in invalid:
            with self.subTest(encoded=encoded), self.assertRaisesRegex(RuntimeError, message):
                MODULE.validate_encoded_input_ids(encoded, "invalid")

    def test_wrong_version_or_non_backend_falcon_fails_closed(self):
        with self.assertRaisesRegex(RuntimeError, "must equal 5.5.0"):
            MODULE.probe_candidates(self.recipes, "4.57.3", FakeAutoTokenizer, FakeBackend)
        FakeAutoTokenizer.backend_ids = set()
        with self.assertRaisesRegex(RuntimeError, "expected TokenizersBackend"):
            MODULE.probe_candidates(self.recipes, "5.5.0", FakeAutoTokenizer, FakeBackend)


if __name__ == "__main__":
    unittest.main()
