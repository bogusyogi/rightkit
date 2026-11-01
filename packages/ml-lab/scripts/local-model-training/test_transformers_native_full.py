import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parent
sys.path.insert(0, str(ROOT))


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


native = load("native_full", ROOT / "train_transformers_native_full.py")
selector = load("backend_selector", ROOT / "select_full_weight_backend.py")
finalizer = load("native_finalizer", ROOT / "finalize_transformers_native_full.py")


class CharacterTokenizer:
    pad_token_id = 0
    eos_token_id = 1

    def apply_chat_template(self, messages, *, tokenize, add_generation_prompt):
        assert tokenize is False
        prompt = f"<user>{messages[0]['content']}</user><assistant>"
        return prompt if add_generation_prompt else prompt + messages[1]["content"] + "</assistant>"

    def __call__(self, text, *, add_special_tokens=False):
        assert add_special_tokens is False
        return {"input_ids": [ord(character) for character in text]}


class NativeFullTrainerContracts(unittest.TestCase):
    def test_native_chat_serialization_masks_every_prompt_token_and_keeps_completion(self):
        tokenizer = CharacterTokenizer()
        recipe = {"serialization": {"kind": "native_chat_template", "fallback": "User: {input}\nAssistant: {output}"}}
        encoded = native.encode_completion_example(tokenizer, recipe, {"input_text": "invoice", "output_text": "Invoice 42"}, 2048)
        first_label = next(index for index, value in enumerate(encoded["labels"]) if value != -100)
        prompt, full = native.render_texts(tokenizer, recipe, "invoice", "Invoice 42")
        self.assertEqual(first_label, len(prompt))
        self.assertEqual(encoded["input_ids"], [ord(character) for character in full])
        self.assertTrue(all(value == -100 for value in encoded["labels"][:first_label]))
        self.assertTrue(all(value != -100 for value in encoded["labels"][first_label:]))

    def test_literal_sotto_serialization_and_dynamic_padding_are_exact(self):
        tokenizer = CharacterTokenizer()
        recipe = {"serialization": {"kind": "literal", "fallback": "### Input\n{input}\n### Output\n{output}"}}
        short = native.encode_completion_example(tokenizer, recipe, {"input_text": "a", "output_text": "b"}, 2048)
        long = native.encode_completion_example(tokenizer, recipe, {"input_text": "alpha", "output_text": "beta"}, 2048)
        padded = native.pad_completion_batch([short, long], tokenizer.pad_token_id)
        self.assertEqual(len(padded["input_ids"][0]), len(padded["input_ids"][1]))
        self.assertEqual(padded["attention_mask"][0][-1], 0)
        self.assertEqual(padded["labels"][0][-1], -100)

    def test_backend_priority_records_failure_then_selects_native_and_all_failure_is_nonfatal(self):
        calls = []
        def probe(backend):
            calls.append(backend)
            if backend == "unsloth_full":
                raise RuntimeError("unsupported architecture")
            return {"optimizer": "torch_adamw", "forward_backward": True, "trainable_fraction": 1.0}
        selected = selector.choose_first_passing(["unsloth_full", "transformers_native_full"], probe)
        self.assertEqual(calls, ["unsloth_full", "transformers_native_full"])
        self.assertEqual(selected["selected"]["backend"], "transformers_native_full")
        self.assertIn("unsupported architecture", selected["attempts"][0]["reason"])
        unsupported = selector.choose_first_passing(["unsloth_full", "transformers_native_full"], lambda backend: (_ for _ in ()).throw(RuntimeError(backend)))
        self.assertEqual(unsupported["status"], "unsupported_for_full_weight_training")
        self.assertIsNone(unsupported["selected"])
        self.assertEqual(len(unsupported["attempts"]), 2)

    def test_native_resume_requires_hash_bound_native_checkpoint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); checkpoint = root / "checkpoint"; checkpoint.mkdir()
            (checkpoint / "model.safetensors").write_bytes(b"weights")
            manifest = root / "train.manifest.json"
            manifest.write_text(json.dumps({"training_backend": "transformers_native_full", "checkpoint_dir": str(checkpoint), "checkpoint_dir_sha256": native.sha_tree(checkpoint), "completed_epochs": 1}), "utf-8")
            resolved, payload = native.resolve_resume(str(manifest))
            self.assertEqual(resolved, checkpoint.resolve())
            self.assertEqual(payload["completed_epochs"], 1)
            (checkpoint / "model.safetensors").write_bytes(b"tampered")
            with self.assertRaisesRegex(RuntimeError, "hash"):
                native.resolve_resume(str(manifest))

    def test_native_finalizer_preserves_model_files_and_excludes_optimizer_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); checkpoint = root / "checkpoint"; checkpoint.mkdir()
            (checkpoint / "model.safetensors").write_bytes(b"weights")
            (checkpoint / "config.json").write_text("{}", "utf-8")
            (checkpoint / "optimizer.pt").write_bytes(b"optimizer")
            selected = root / "train.manifest.json"
            selected.write_text(json.dumps({"training_backend": "transformers_native_full", "checkpoint_dir": str(checkpoint), "checkpoint_dir_sha256": native.sha_tree(checkpoint)}), "utf-8")
            manifest_path = finalizer.finalize(selected, root / "final", "granite")
            manifest = json.loads(manifest_path.read_text("utf-8")); merged = Path(manifest["merged_dir"])
            self.assertTrue((merged / "model.safetensors").is_file())
            self.assertTrue((merged / "config.json").is_file())
            self.assertFalse((merged / "optimizer.pt").exists())
            self.assertEqual(manifest["merged_dir_sha256"], native.sha_tree(merged))
            self.assertFalse(manifest["adapter_merge_required"])

    def test_granite_recipe_is_native_first_and_exact_transformers_pin_is_present(self):
        recipes = json.loads((ROOT / "model-recipes.json").read_text("utf-8"))
        granite = recipes["families"]["granite"]
        self.assertEqual(granite["training_backend"]["priority"], ["transformers_native_full", "unsloth_full"])
        self.assertEqual(granite["training_backend"]["native_transformers_version"], "5.5.0")
        self.assertEqual(granite["training_backend"]["architecture"], "GraniteMoeHybridForCausalLM")
        requirements = (ROOT / "requirements.vast-cu126.pins.txt").read_text("utf-8")
        self.assertIn("transformers==5.5.0", requirements)
        self.assertNotIn("transformers==4.57.3", requirements)


if __name__ == "__main__":
    unittest.main()
