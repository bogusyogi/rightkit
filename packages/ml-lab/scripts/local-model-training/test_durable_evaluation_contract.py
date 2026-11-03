import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("train_launcher", ROOT / "train.py")
train = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(train)


def tree_sha(path: Path) -> str:
    entries = []
    for item in sorted(path.rglob("*")):
        if item.is_file():
            entries.append([item.relative_to(path).as_posix(), item.stat().st_size, train.file_sha256(item)])
    return "sha256:" + hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()


class DurableEvaluationContractTests(unittest.TestCase):
    def test_wrapper_exit_retains_coherent_view_and_validate_consumes_it(self):
        with tempfile.TemporaryDirectory(prefix="heardright-durable-eval-") as directory:
            root = Path(directory)
            source = root / "frozen.jsonl"
            source.write_text(json.dumps({"row_id": "gold-1", "input_text": "Keep Acme 42."}) + "\n", "utf-8")
            source_before = source.read_bytes()
            output = root / "evaluation"
            checkpoint = root / "checkpoint"
            checkpoint.mkdir()
            (checkpoint / "weights.bin").write_bytes(b"checkpoint")

            def opaque_evaluator(*_args, **_kwargs):
                view = Path(train.argument_value(train.sys.argv, "--eval-input")).resolve()
                evaluation = output / "evaluation.json"
                row_hash = "sha256:" + hashlib.sha256(b"Keep Acme 42.").hexdigest()
                evaluation.write_text(json.dumps({
                    "schema_version": "heardright.l1.evaluation.v1",
                    "test_only": True,
                    "checkpoint_dir": str(checkpoint.resolve()),
                    "checkpoint_dir_sha256": tree_sha(checkpoint),
                    "eval_input": str(view),
                    "eval_input_sha256": train.file_sha256(view),
                    "rows": [{"id": "gold-1", "input_sha256": row_hash, "output_text": "Keep Acme 42.", "validation": {"ok": True, "errors": []}}],
                    "aggregate": {"rows": 1, "safety": {"deletion": 0, "invention": 0, "protected_span": 0, "no_op": 0}, "quality": {"correction_misses": 0}},
                }), "utf-8")
                (output / "evaluate.manifest.json").write_text(json.dumps({
                    "schema_version": "heardright.l1.action_manifest.v1",
                    "action": "evaluate",
                    "evaluation_artifact": str(evaluation),
                    "evaluation_sha256": "sha256:opaque-pre-normalization",
                }), "utf-8")

            prior_argv = train.sys.argv[:]
            try:
                train.sys.argv = ["train.py", "--family", "gemma", "--action", "evaluate", "--output-dir", str(output), "--checkpoint-artifact", str(root / "checkpoint.manifest.json"), "--eval-input", str(source)]
                with patch.object(train.runpy, "run_path", side_effect=opaque_evaluator):
                    train.main()
            finally:
                train.sys.argv = prior_argv

            view = output / "normalized-evaluation.jsonl"
            evaluation_path = output / "evaluation.json"
            manifest_path = output / "evaluate.manifest.json"
            evaluation = json.loads(evaluation_path.read_text("utf-8"))
            manifest = json.loads(manifest_path.read_text("utf-8"))
            self.assertTrue(view.is_file())
            self.assertEqual(source.read_bytes(), source_before)
            self.assertEqual(evaluation["eval_input"], str(view.resolve()))
            self.assertEqual(evaluation["eval_input_sha256"], train.file_sha256(view))
            self.assertEqual(evaluation["source_eval_input_sha256"], train.file_sha256(source))
            self.assertEqual(manifest["evaluation_sha256"], train.file_sha256(evaluation_path))
            self.assertEqual(manifest["eval_input_sha256"], train.file_sha256(view))

            validation_output = root / "validate"
            result = subprocess.run([
                sys.executable, str(ROOT / "train.py"), "--family", "gemma", "--action", "validate",
                "--output-dir", str(validation_output), "--validation-artifact", str(evaluation_path),
                "--eval-input", str(view),
            ], text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            validation_manifest = json.loads((validation_output / "validate.manifest.json").read_text("utf-8"))
            self.assertTrue(validation_manifest["q8_validated"])
            self.assertEqual(validation_manifest["validation_artifact"], str(evaluation_path.resolve()))
            self.assertEqual(validation_manifest["validation_sha256"], train.file_sha256(evaluation_path))


if __name__ == "__main__":
    unittest.main()
