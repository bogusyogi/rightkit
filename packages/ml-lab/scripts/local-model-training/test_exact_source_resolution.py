import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("resolve_exact_model_sources", ROOT / "resolve_exact_model_sources.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SourceResolutionTests(unittest.TestCase):
    def setUp(self):
        self.recipes = json.loads((ROOT / "model-recipes.json").read_text("utf-8"))

    def test_resolves_six_candidates_plus_gemma_preflight_to_exact_snapshots(self):
        with tempfile.TemporaryDirectory() as root:
            def downloader(*, repo_id, revision):
                path = Path(root) / revision
                path.mkdir(exist_ok=True)
                return str(path)
            entries = MODULE.resolve_sources(self.recipes, downloader)
            self.assertEqual(len(entries), 7)
            self.assertEqual(entries[-1]["lane"], "gemma_grmr")
            self.assertTrue(all(Path(item["resolved_path"]).name == item["revision"] for item in entries))

    def test_rejects_unbound_snapshot_path(self):
        with tempfile.TemporaryDirectory() as root:
            wrong = Path(root) / "main"
            wrong.mkdir()
            with self.assertRaisesRegex(RuntimeError, "exact revision"):
                MODULE.resolve_sources(self.recipes, lambda **_kwargs: str(wrong))


if __name__ == "__main__":
    unittest.main()
