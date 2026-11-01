import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import finalize_remote_run_bundle as bundle


class FinalizeRemoteRunBundleTests(unittest.TestCase):
    def test_terminal_bundle_keeps_deployment_artifacts_and_excludes_synced_checkpoints(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / "run"
            merged = run / "falcon" / "full-merge" / "merged"
            checkpoint = run / "falcon" / "full-epoch-1" / "checkpoint-94"
            exported = run / "falcon" / "export-q8"
            receipts = run / "stage-receipts"
            for path in (merged, checkpoint, exported, receipts):
                path.mkdir(parents=True, exist_ok=True)
            (run / "state.json").write_text(
                json.dumps({"status": "complete"}),
                "utf-8",
            )
            (merged / "model.safetensors").write_bytes(b"merged")
            (checkpoint / "model.safetensors").write_bytes(b"synced")
            (exported / "model.gguf").write_bytes(b"gguf")
            (receipts / "falcon--gguf-evaluate.json").write_text("{}\n", "utf-8")
            archive = root / "final.tar.gz"
            manifest_path = root / "final.manifest.json"
            result = bundle.build_bundle(run, archive, manifest_path)
            names = {entry["path"] for entry in result["files"]}
            self.assertEqual(result["terminal_status"], "complete")
            self.assertIn("falcon/full-merge/merged/model.safetensors", names)
            self.assertIn("falcon/export-q8/model.gguf", names)
            self.assertNotIn("falcon/full-epoch-1/checkpoint-94/model.safetensors", names)
            with tarfile.open(archive, "r:gz") as handle:
                self.assertEqual(set(handle.getnames()), names)

    def test_nonterminal_run_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / "run"
            run.mkdir()
            (run / "state.json").write_text(
                json.dumps({"status": "running"}),
                "utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "not terminal"):
                bundle.build_bundle(run, root / "final.tar.gz", root / "manifest.json")

    def test_completed_lane_can_sync_before_terminal_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / "run"
            merged = run / "falcon" / "full-merge" / "merged"
            exported = run / "falcon" / "export-q8"
            receipts = run / "stage-receipts"
            for path in (merged, exported, receipts):
                path.mkdir(parents=True, exist_ok=True)
            (run / "state.json").write_text(
                json.dumps({"status": "running"}),
                "utf-8",
            )
            (merged / "model.safetensors").write_bytes(b"merged")
            (exported / "model.gguf").write_bytes(b"gguf")
            (receipts / "falcon--gguf-evaluate.json").write_text("{}\n", "utf-8")
            result = bundle.build_bundle(
                run,
                root / "falcon.tar.gz",
                root / "falcon.manifest.json",
                "falcon",
            )
            self.assertEqual(result["schema_version"], "heardright.l1.lane_transfer.v1")
            self.assertEqual(result["lane"], "falcon")
            self.assertTrue(all(
                entry["path"] == "state.json"
                or entry["path"].startswith("falcon/")
                or entry["path"].startswith("stage-receipts/falcon--")
                for entry in result["files"]
            ))


if __name__ == "__main__":
    unittest.main()
