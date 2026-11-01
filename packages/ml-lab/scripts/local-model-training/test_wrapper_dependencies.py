from __future__ import annotations

import hashlib
import os
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(os.environ.get("RIGHTKIT_ML_BUNDLE_SCRIPTS", Path(__file__).parent)).resolve()
DEPENDENCIES = {
    "capture_native_assets.py": ("__pycache__/capture_native_assets.cpython-311.pyc", "cdd61fc1107257035daf3882232187c310bf8da2e235951a59a34cb36049f88f"),
    "convert_hf_to_q8.py": ("__pycache__/convert_hf_to_q8.cpython-311.pyc", "6647babe21865f255954d620b772e565b78fbb71b2b4cfe13d18ed21990ddf64"),
    "vast_shutdown_hook.py": ("__pycache__/vast_shutdown_hook.cpython-311.pyc", "74048c82da20d5994e732020bef134efb0ed43c0cfc43d3d8e3f65459f761633"),
    "train.py": ("recovered_train_runner.pyc", "a911da2dabee3c9333e11f7e8cfd9baab164816b81505722e0af22cf4dfed6dd"),
}


class WrapperDependencyTests(unittest.TestCase):
    def test_exact_recovered_artifacts_exist_and_match(self):
        self.assertEqual(sys.version_info[:2], (3, 11))
        for wrapper, (dependency, expected) in DEPENDENCIES.items():
            self.assertTrue((ROOT / wrapper).is_file(), wrapper)
            artifact = ROOT / dependency
            self.assertTrue(artifact.is_file(), dependency)
            self.assertEqual(hashlib.sha256(artifact.read_bytes()).hexdigest(), expected, dependency)

    def test_every_wrapper_help_executes_from_the_assembled_root(self):
        for wrapper in DEPENDENCIES:
            completed = subprocess.run([sys.executable, str(ROOT / wrapper), "--help"], text=True, capture_output=True, timeout=30)
            self.assertEqual(completed.returncode, 0, f"{wrapper}: {completed.stderr}")
            self.assertIn("usage:", completed.stdout.lower(), wrapper)

    def test_recovered_trainer_static_sibling_dependencies_exist(self):
        for name in ["model-recipes.json", "validate_output.mjs", "mock_child.mjs"]:
            self.assertTrue((ROOT / name).is_file(), name)


if __name__ == "__main__":
    unittest.main()
