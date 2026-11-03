import importlib.util
import json
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

ROOT = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("train_launcher", ROOT / "train.py")
train = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(train)


class EvaluationNormalizationTests(unittest.TestCase):
    def write_rows(self, directory, rows):
        path = Path(directory) / "frozen.jsonl"
        path.write_text("".join(json.dumps(row) + "\n" for row in rows), "utf-8")
        return path

    def test_row_id_view_adds_matching_id_without_mutating_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.write_rows(directory, [{"row_id": "gold-1", "input_text": "hello", "output_text": "hello"}])
            original = source.read_bytes()
            output = Path(directory) / "evaluation"
            view, source_sha256 = train.normalized_evaluation_view(source, output)
            self.assertEqual(json.loads(view.read_text("utf-8"))["id"], "gold-1")
            self.assertEqual(source.read_bytes(), original)
            self.assertTrue(view.exists())
            self.assertEqual(view.parent, output.resolve())
            self.assertEqual(source_sha256, train.file_sha256(source))

    def test_legacy_id_is_preserved_and_conflict_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.write_rows(directory, [{"id": "legacy-1", "input_text": "hello"}])
            view, _ = train.normalized_evaluation_view(source, Path(directory) / "legacy-evaluation")
            self.assertEqual(json.loads(view.read_text("utf-8"))["id"], "legacy-1")
            conflict = self.write_rows(directory, [{"id": "legacy-1", "row_id": "gold-1", "input_text": "hello"}])
            with self.assertRaisesRegex(ValueError, "conflicting"):
                train.normalized_evaluation_view(conflict, Path(directory) / "conflict-evaluation")

    def test_evaluate_argv_rewrites_only_eval_input(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.write_rows(directory, [{"row_id": "gold-1", "input_text": "hello"}])
            rewritten = train.rewrite_evaluation_argv(["train.py", "--action", "evaluate", "--eval-input", str(source), "--checkpoint-artifact", "checkpoint"], Path(directory) / "view.jsonl")
            self.assertEqual(rewritten[rewritten.index("--eval-input") + 1], str(Path(directory) / "view.jsonl"))
            self.assertEqual(rewritten[-1], "checkpoint")

    def test_evaluate_action_retains_and_binds_normalized_view_after_opaque_runner_exits(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.write_rows(directory, [{"row_id": "gold-1", "input_text": "hello"}])
            output = Path(directory) / "evaluation"
            original, captured = source.read_bytes(), {}
            def opaque_runner(*_args, **_kwargs):
                captured["argv"] = train.sys.argv[:]
                view = Path(captured["argv"][captured["argv"].index("--eval-input") + 1]).resolve()
                evaluation = output / "evaluation.json"
                evaluation.write_text(json.dumps({"schema_version": "heardright.l1.evaluation.v1", "eval_input": str(view), "eval_input_sha256": train.file_sha256(view), "rows": [], "aggregate": {"rows": 0}}), "utf-8")
                (output / "evaluate.manifest.json").write_text(json.dumps({"evaluation_artifact": str(evaluation), "evaluation_sha256": "sha256:stale"}), "utf-8")
            prior_argv = train.sys.argv[:]
            try:
                train.sys.argv = ["train.py", "--action", "evaluate", "--output-dir", str(output), "--eval-input", str(source), "--checkpoint-artifact", "checkpoint"]
                with patch.object(train.runpy, "run_path", side_effect=opaque_runner):
                    train.main()
            finally:
                train.sys.argv = prior_argv
            view = Path(captured["argv"][captured["argv"].index("--eval-input") + 1])
            self.assertNotEqual(view, source)
            self.assertTrue(view.exists())
            self.assertEqual(view.parent, output.resolve())
            self.assertEqual(source.read_bytes(), original)
            evaluation_path = output / "evaluation.json"
            evaluation = json.loads(evaluation_path.read_text("utf-8"))
            manifest = json.loads((output / "evaluate.manifest.json").read_text("utf-8"))
            self.assertEqual(evaluation["eval_input"], str(view.resolve()))
            self.assertEqual(evaluation["eval_input_sha256"], train.file_sha256(view))
            self.assertEqual(evaluation["source_eval_input_sha256"], train.file_sha256(source))
            self.assertEqual(manifest["evaluation_sha256"], train.file_sha256(evaluation_path))
            self.assertEqual(manifest["eval_input_sha256"], train.file_sha256(view))


if __name__ == "__main__":
    unittest.main()
