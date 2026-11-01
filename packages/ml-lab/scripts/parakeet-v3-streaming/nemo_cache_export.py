"""Shared NeMo-native cache-aware encoder export configuration.

NeMo 2.7.3 rebuilds ``encoder.streaming_cfg`` when cache export support is
enabled. Keep the order here deliberate: select the attention context, enable
NeMo's export forward, then apply the locked 10/2 profile last.
"""
from __future__ import annotations

from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


CHUNK_SIZE_FRAMES = 50
SHIFT_SIZE_FRAMES = 25
CACHE_LEFT_FRAMES = 125
CACHE_RIGHT_FRAMES = 25
MAX_CONTEXT_FRAMES = 125
ONNX_OPSET = 17


@dataclass(frozen=True)
class CacheExportContract:
    attention_context: tuple[int, int]
    chunk_size_frames: int
    shift_size_frames: int
    cache_last_channel_frames: int
    cache_drop_size: int
    valid_out_len: int
    input_feature_frames: int
    pre_encode_cache_frames: int

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _time_value(value: Any) -> int:
    if isinstance(value, (list, tuple)):
        return int(value[-1])
    return int(value)


def configure_cache_export(model: Any) -> CacheExportContract:
    """Enable NeMo's supported cache export with the locked 10/2 profile."""

    encoder = model.encoder
    required = {
        "model.set_export_config": getattr(model, "set_export_config", None),
        "encoder.set_default_att_context_size": getattr(
            encoder, "set_default_att_context_size", None
        ),
        "encoder.setup_streaming_params": getattr(encoder, "setup_streaming_params", None),
        "encoder.export": getattr(encoder, "export", None),
    }
    missing = [name for name, value in required.items() if not callable(value)]
    if missing:
        raise RuntimeError(f"pinned NeMo cache export API missing: {', '.join(missing)}")

    attention_context = [CACHE_LEFT_FRAMES, CACHE_RIGHT_FRAMES]
    encoder.set_default_att_context_size(attention_context)
    model.set_export_config({"cache_support": True})
    encoder.setup_streaming_params(
        chunk_size=CHUNK_SIZE_FRAMES,
        shift_size=SHIFT_SIZE_FRAMES,
        left_chunks=None,
        att_context_size=attention_context,
        max_context=MAX_CONTEXT_FRAMES,
    )

    if list(encoder.att_context_size) != attention_context:
        raise RuntimeError(
            f"effective attention context drifted: {encoder.att_context_size!r}"
        )
    if not bool(getattr(encoder, "export_cache_support", False)):
        raise RuntimeError("NeMo cache export support was not enabled")

    cfg = encoder.streaming_cfg
    cache_frames = int(cfg.last_channel_cache_size)
    cache_drop_size = int(cfg.cache_drop_size)
    valid_out_len = int(cfg.valid_out_len)
    input_feature_frames = _time_value(cfg.chunk_size) + _time_value(
        cfg.pre_encode_cache_size
    )
    pre_encode_cache_frames = _time_value(cfg.pre_encode_cache_size)
    expected = {
        "cache_last_channel_frames": (cache_frames, CACHE_LEFT_FRAMES),
        "cache_drop_size": (cache_drop_size, CHUNK_SIZE_FRAMES - SHIFT_SIZE_FRAMES),
        "valid_out_len": (valid_out_len, SHIFT_SIZE_FRAMES),
    }
    drift = [f"{name}={actual} expected={wanted}" for name, (actual, wanted) in expected.items() if actual != wanted]
    if drift:
        raise RuntimeError("NeMo streaming profile drifted: " + "; ".join(drift))

    return CacheExportContract(
        attention_context=(CACHE_LEFT_FRAMES, CACHE_RIGHT_FRAMES),
        chunk_size_frames=CHUNK_SIZE_FRAMES,
        shift_size_frames=SHIFT_SIZE_FRAMES,
        cache_last_channel_frames=cache_frames,
        cache_drop_size=cache_drop_size,
        valid_out_len=valid_out_len,
        input_feature_frames=input_feature_frames,
        pre_encode_cache_frames=pre_encode_cache_frames,
    )


def nemo_input_example(encoder: Any):
    """Return NeMo's own batch-first cache-aware example tensors."""

    return encoder.input_example(max_batch=1, max_dim=CACHE_LEFT_FRAMES)


def export_with_nemo(
    encoder: Any,
    output: str | Path,
    *,
    input_example=None,
    onnx_opset_version: int = ONNX_OPSET,
):
    """Export through ``Exportable.export`` so ``forward_for_export`` is used."""

    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    if input_example is None:
        input_example = nemo_input_example(encoder)
    return encoder.export(
        str(output),
        input_example=input_example,
        onnx_opset_version=onnx_opset_version,
        check_trace=False,
    )
