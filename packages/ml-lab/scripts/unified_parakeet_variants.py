from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

DEFAULT_MAX_TOKENS = 10
WEIGHT_ENCODER_DML_BLOCKLIST = ("bits2", "b16_", "b256_")

@dataclass(frozen=True)
class Variant:
    name: str
    encoder: Path
    decoder: Path
    tokenizer: Path
    encoder_provider: str
    decoder_provider: str
    max_tokens: int = DEFAULT_MAX_TOKENS
    static_window: int = 0


def variants(cand: Path, include_mixed: bool) -> list[Variant]:
    specs: list[tuple[str, Path, Path, Path]] = [
        (
            "int8enc_int8dec",
            cand / "onnx_int8" / "encoder.int8.onnx",
            cand / "onnx_int8" / "decoder_joint.int8.onnx",
            cand / "onnx_int8" / "tokenizer.model",
        ),
        (
            "fp32enc_int8dec",
            cand / "onnx_fp32" / "encoder.onnx",
            cand / "onnx_int8" / "decoder_joint.int8.onnx",
            cand / "onnx_int8" / "tokenizer.model",
        ),
        (
            "fp32enc_fp32dec",
            cand / "onnx_fp32" / "encoder.onnx",
            cand / "onnx_fp32" / "decoder_joint.onnx",
            cand / "onnx_fp32" / "tokenizer.model",
        ),
    ]
    naive = cand / "onnx_fp16" / "encoder.fp16_naive.onnx"
    if include_mixed and naive.exists():
        specs.extend(
            [
                (
                    "fp16naiveenc_int8dec",
                    naive,
                    cand / "onnx_int8" / "decoder_joint.int8.onnx",
                    cand / "onnx_int8" / "tokenizer.model",
                ),
                (
                    "fp16naiveenc_fp32dec",
                    naive,
                    cand / "onnx_fp32" / "decoder_joint.onnx",
                    cand / "onnx_fp32" / "tokenizer.model",
                ),
            ]
        )
    mixed = cand / "onnx_mixed_fp16" / "encoder.mixed_fp16.onnx"
    if include_mixed and mixed.exists():
        specs.extend(
            [
                (
                    "mixedenc_int8dec",
                    mixed,
                    cand / "onnx_int8" / "decoder_joint.int8.onnx",
                    cand / "onnx_int8" / "tokenizer.model",
                ),
                (
                    "mixedenc_fp32dec",
                    mixed,
                    cand / "onnx_fp32" / "decoder_joint.onnx",
                    cand / "onnx_fp32" / "tokenizer.model",
                ),
            ]
        )
    weight_dir = cand / "onnx_weight4"
    if include_mixed and weight_dir.exists():
        for weight4 in sorted(weight_dir.glob("encoder.*.onnx")):
            name = weight4.stem.removeprefix("encoder.")
            specs.append(
                (
                    f"weight4_{name}_enc_int8dec",
                    weight4,
                    cand / "onnx_int8" / "decoder_joint.int8.onnx",
                    cand / "onnx_int8" / "tokenizer.model",
                )
            )
    static_dir = cand / "onnx_static"
    if include_mixed and static_dir.exists():
        mixed_static = static_dir / "encoder.mixed_fp16.static1500.onnx"
        if mixed_static.exists():
            specs.append(
                (
                    "static_mixedenc_int8dec",
                    mixed_static,
                    cand / "onnx_int8" / "decoder_joint.int8.onnx",
                    cand / "onnx_int8" / "tokenizer.model",
                )
            )
        for weight4 in sorted(static_dir.glob("encoder.static*.onnx")):
            if weight4.stem.startswith("encoder.static1500."):
                name = weight4.stem.removeprefix("encoder.static1500.")
                base_name = f"static_weight4_{name}_enc_int8dec"
            elif weight4.stem.startswith("encoder.static3000."):
                name = weight4.stem.removeprefix("encoder.static3000.")
                base_name = f"static3000_weight4_{name}_enc_int8dec"
            else:
                continue
            specs.append(
                (
                    base_name,
                    weight4,
                    cand / "onnx_int8" / "decoder_joint.int8.onnx",
                    cand / "onnx_int8" / "tokenizer.model",
                )
            )

    out: list[Variant] = []
    for base, enc, dec, tok in specs:
        for ep in ("dml", "cpu"):
            if ep == "dml" and any(mark in base for mark in WEIGHT_ENCODER_DML_BLOCKLIST):
                continue
            for dp in ("cpu", "dml"):
                out.append(
                    Variant(
                        name=f"{base}__enc_{ep}__dec_{dp}",
                        encoder=enc,
                        decoder=dec,
                        tokenizer=tok,
                        encoder_provider=ep,
                        decoder_provider=dp,
                        static_window=3000 if base.startswith("static3000_") else 1500 if base.startswith("static_") else 0,
                    )
                )
    return out
