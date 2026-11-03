import unittest

from autotune_fullft_batch import build_receipt, select_configuration


class AutotuneSelectionTests(unittest.TestCase):
    def test_selects_largest_batch_within_memory_and_throughput(self):
        rows = [
            {"device_batch": 2, "status": "pass", "peak_vram_gib": 10, "tokens_per_second": 100},
            {"device_batch": 4, "status": "pass", "peak_vram_gib": 14, "tokens_per_second": 105},
            {"device_batch": 8, "status": "pass", "peak_vram_gib": 19.9, "tokens_per_second": 91},
            {"device_batch": 16, "status": "pass", "peak_vram_gib": 20.1, "tokens_per_second": 120},
        ]
        selected = select_configuration(rows, effective_batch=16, peak_limit_gib=20, max_regression=0.1)
        self.assertEqual(selected["device_batch"], 8)
        self.assertEqual(selected["gradient_accumulation"], 2)

    def test_oom_and_throughput_regression_roll_back(self):
        rows = [
            {"device_batch": 2, "status": "pass", "peak_vram_gib": 10, "tokens_per_second": 100},
            {"device_batch": 4, "status": "pass", "peak_vram_gib": 12, "tokens_per_second": 89.9},
            {"device_batch": 8, "status": "oom"},
            {"device_batch": 16, "status": "oom"},
        ]
        selected = select_configuration(rows, effective_batch=16, peak_limit_gib=20, max_regression=0.1)
        self.assertEqual(selected["device_batch"], 2)
        self.assertEqual(selected["gradient_accumulation"], 8)

    def test_batch_one_is_a_valid_full_weight_fallback(self):
        rows = [
            {"device_batch": 1, "status": "pass", "peak_vram_gib": 18, "tokens_per_second": 50},
            {"device_batch": 2, "status": "oom"},
        ]
        selected = select_configuration(rows, effective_batch=16, peak_limit_gib=20, max_regression=0.1)
        self.assertEqual(selected["device_batch"], 1)
        self.assertEqual(selected["gradient_accumulation"], 16)

    def test_selection_failure_is_persistable_as_a_diagnostic_receipt(self):
        receipt = build_receipt(
            family="granite", model_source="ibm/granite", model_revision="revision",
            training_backend="transformers_native_full", optimizer="adamw_8bit",
            training_input_sha256="sha256:input", recipes_sha256="sha256:recipes",
            gpu={"name": "RTX", "total_vram_gib": 24}, policy={},
            results=[{"device_batch": 1, "status": "oom"}], selected=None,
            selection_error="no microbatch satisfies the full-weight resource policy",
        )
        self.assertEqual(receipt["status"], "no_safe_microbatch")
        self.assertEqual(receipt["selection_error"], "no microbatch satisfies the full-weight resource policy")


if __name__ == "__main__":
    unittest.main()
