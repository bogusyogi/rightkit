import importlib.util, json, tempfile, unittest
from pathlib import Path
MODULE_PATH = Path(__file__).with_name("prepare_source_checkpoint.py")
SPEC = importlib.util.spec_from_file_location("prepare_source_checkpoint", MODULE_PATH); MODULE = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(MODULE)

class TestSourceCheckpoint(unittest.TestCase):
    def test_sha_tree_is_deterministic_and_content_bound(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root); (path/"b").write_text("two"); (path/"a").write_text("one")
            first=MODULE.sha_tree(path); self.assertEqual(first, MODULE.sha_tree(path)); (path/"a").write_text("changed"); self.assertNotEqual(first, MODULE.sha_tree(path))

if __name__ == "__main__": unittest.main()
