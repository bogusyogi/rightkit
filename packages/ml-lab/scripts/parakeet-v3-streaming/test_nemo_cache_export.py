from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import numpy as np
import torch

from nemo_cache_export import (
    CACHE_LEFT_FRAMES,
    CACHE_RIGHT_FRAMES,
    CHUNK_SIZE_FRAMES,
    SHIFT_SIZE_FRAMES,
    configure_cache_export,
    export_with_nemo,
)
from stage_d_export_coreml_v2 import parity_rows, static_scalar_value


class _StreamingConfig:
    chunk_size = [393, 400]
    shift_size = [193, 200]
    cache_drop_size = 25
    last_channel_cache_size = 125
    valid_out_len = 25
    pre_encode_cache_size = [0, 9]


class _FakeEncoder:
    def __init__(self) -> None:
        self.events: list[tuple[str, object]] = []
        self.att_context_size = [-1, -1]
        self.streaming_cfg = None
        self.export_cache_support = False
        self.eager_calls = 0

    def set_default_att_context_size(self, value) -> None:
        self.events.append(("set_default_att_context_size", list(value)))
        self.att_context_size = list(value)

    def setup_streaming_params(self, **kwargs) -> None:
        self.events.append(("setup_streaming_params", dict(kwargs)))
        self.streaming_cfg = _StreamingConfig()

    def export(self, output: str, **kwargs):
        self.events.append(("export", (output, kwargs)))
        Path(output).touch()
        return [output], ["fake export"]

    def input_example(self, **kwargs):
        self.events.append(("input_example", dict(kwargs)))
        return ("nemo-owned-example",)

    def __call__(self, *args, **kwargs):
        self.eager_calls += 1
        raise AssertionError("ordinary eager forward must not drive a cache export")


class _FakeModel:
    def __init__(self) -> None:
        self.encoder = _FakeEncoder()

    def set_export_config(self, config) -> None:
        self.encoder.events.append(("set_export_config", dict(config)))
        self.encoder.export_cache_support = bool(config["cache_support"])
        # NeMo 2.7.3 rebuilds streaming_cfg here. The exact profile therefore
        # has to be applied after this call.
        self.encoder.streaming_cfg = "reset-by-set-export-config"


class NemoCacheExportTests(unittest.TestCase):
    def test_configures_export_then_reapplies_locked_streaming_profile(self) -> None:
        model = _FakeModel()

        contract = configure_cache_export(model)

        self.assertEqual(
            [event[0] for event in model.encoder.events],
            [
                "set_default_att_context_size",
                "set_export_config",
                "setup_streaming_params",
            ],
        )
        setup = model.encoder.events[-1][1]
        self.assertEqual(setup["chunk_size"], CHUNK_SIZE_FRAMES)
        self.assertEqual(setup["shift_size"], SHIFT_SIZE_FRAMES)
        self.assertEqual(setup["att_context_size"], [CACHE_LEFT_FRAMES, CACHE_RIGHT_FRAMES])
        self.assertEqual(contract.valid_out_len, 25)
        self.assertEqual(contract.cache_last_channel_frames, 125)

    def test_exports_through_nemo_exportable_not_ordinary_forward(self) -> None:
        model = _FakeModel()
        configure_cache_export(model)

        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "AudioEncoderCacheExplicit.onnx"
            export_with_nemo(model.encoder, output, onnx_opset_version=17)
            self.assertTrue(output.is_file())

        self.assertEqual(model.encoder.eager_calls, 0)
        self.assertEqual(model.encoder.events[-2][0], "input_example")
        self.assertEqual(model.encoder.events[-1][0], "export")

    def test_torchscript_parity_uses_declared_rtol_and_atol(self) -> None:
        expected = (torch.tensor([1.0, 0.0]),)
        within = (torch.tensor([1.00025, 0.00009]),)
        outside = (torch.tensor([1.00025, 0.00025]),)

        self.assertTrue(parity_rows(within, expected)[0]["match"])
        self.assertFalse(parity_rows(outside, expected)[0]["match"])

    def test_coreml_static_cast_accepts_only_singleton_arrays(self) -> None:
        self.assertEqual(static_scalar_value(np.array([17], dtype=np.int64)), 17)
        self.assertEqual(static_scalar_value(np.array(19, dtype=np.int64)), 19)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            static_scalar_value(np.array([1, 2], dtype=np.int64))


if __name__ == "__main__":
    unittest.main()
