import os
from pathlib import Path
import tempfile
import time
import unittest

import paid_run_watchdog as watchdog


class PaidRunWatchdogTests(unittest.TestCase):
    def base(self):
        return {
            "now": 300,
            "installed_at": 0,
            "state": {"status": "running"},
            "matching_pids": [123],
            "progress_mtime": 280,
            "gpu_util": 20,
            "workload_cpu_util": 80,
            "free_disk_bytes": 10 * 1024**3,
            "startup_grace_seconds": 180,
            "progress_timeout_seconds": 900,
            "terminal_grace_seconds": 600,
            "min_free_bytes": 5 * 1024**3,
        }

    def test_active_gpu_and_fresh_log_continue(self):
        self.assertEqual(watchdog.decision(**self.base())["action"], "continue")

    def test_authorized_missing_process_stops_after_startup_grace(self):
        values = self.base()
        values["matching_pids"] = []
        result = watchdog.decision(**values)
        self.assertEqual(result["action"], "stop")
        self.assertEqual(result["reason"], "authorized_run_process_missing")

    def test_matching_launcher_process_is_protected_during_startup_grace(self):
        values = self.base()
        values.update({
            "now": 2,
            "matching_pids": [123],
            "progress_mtime": -3600,
            "gpu_util": 0,
            "workload_cpu_util": 0,
        })
        result = watchdog.decision(**values)
        self.assertEqual(result["action"], "wait")
        self.assertEqual(result["reason"], "startup_grace")

    def test_low_gpu_and_stale_progress_stops(self):
        values = self.base()
        values.update({
            "progress_mtime": values["now"] - 901,
            "gpu_util": 0,
            "workload_cpu_util": 0,
        })
        result = watchdog.decision(**values)
        self.assertEqual(
            result["reason"],
            "run_stalled_without_gpu_or_file_progress",
        )

    def test_gpu_activity_protects_long_stage_with_sparse_logs(self):
        values = self.base()
        values.update({
            "progress_mtime": values["now"] - 3600,
            "gpu_util": 95,
            "workload_cpu_util": 0,
        })
        self.assertEqual(watchdog.decision(**values)["action"], "continue")

    def test_cpu_evaluation_protects_long_stage_with_sparse_logs(self):
        values = self.base()
        values.update({
            "progress_mtime": values["now"] - 3600,
            "gpu_util": 0,
            "workload_cpu_util": 1700,
        })
        self.assertEqual(watchdog.decision(**values)["action"], "continue")

    def test_disk_reserve_breach_stops_even_with_active_gpu(self):
        values = self.base()
        values["free_disk_bytes"] = 4 * 1024**3
        result = watchdog.decision(**values)
        self.assertEqual(result["reason"], "disk_reserve_breached")

    def test_terminal_state_has_bounded_sync_grace(self):
        values = self.base()
        values.update({
            "state": {"status": "complete"},
            "progress_mtime": values["now"] - 100,
        })
        self.assertEqual(watchdog.decision(**values)["action"], "wait")
        values["progress_mtime"] = values["now"] - 601
        self.assertEqual(
            watchdog.decision(**values)["reason"],
            "terminal_sync_grace_exhausted",
        )

    def test_verified_terminal_sync_stops_immediately(self):
        values = self.base()
        values["state"] = {
            "status": "complete",
            "final_sync_verified": True,
        }
        result = watchdog.decision(**values)
        self.assertEqual(result["action"], "stop")
        self.assertEqual(result["reason"], "terminal_sync_verified")

    def test_stage_log_counts_as_progress(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage-receipts"
            stage.mkdir()
            path = stage / "falcon--train.log"
            path.write_text("step 1\n", "utf-8")
            self.assertGreaterEqual(
                watchdog.latest_progress_mtime(root),
                path.stat().st_mtime,
            )

    def test_default_progress_timeout_covers_verified_checkpoint_transfer(self):
        with tempfile.TemporaryDirectory() as directory:
            args = watchdog.parse_args([
                "--watch",
                "--run-root",
                directory,
                "--command-token",
                "/workspace/heardright-run",
                "--instance-id",
                "45523748",
            ])
            self.assertEqual(args.progress_timeout_seconds, 1800)


if __name__ == "__main__":
    unittest.main()
