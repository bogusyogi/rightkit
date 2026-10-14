#!/usr/bin/env python3
"""Export local Qwen3-TTS 12Hz 1.7B weights for rightkit-tts-qwen 0.2.0.

Requires Python >=3.10, numpy & gguf>=0.18.0; no Torch or network access.
GGUF serialization follows vendored ggml/include/gguf.h. Quantization uses
the same gguf.quants.quantize API as QTS v0.2.0's upstream exporter.
"""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import struct
import sys
import tempfile
from typing import Any, BinaryIO

import numpy as np
import gguf


ARCH = "qwen3-tts"
ALIGNMENT = 32
CHUNK_BYTES = 8 * 1024 * 1024  # Bound conversion memory, including text embeddings.
U32, I32, F32, STRING, ARRAY = 4, 5, 6, 8, 9  # gguf_type, gguf.h
Metadata = dict[str, tuple[int, Any]]


def load_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def u32(value: Any) -> int:
    if type(value) is not int or not 0 <= value <= 0xFFFFFFFF:
        raise ValueError(f"expected U32, got {value!r}")
    return value


def string_bytes(value: str) -> bytes:
    encoded = value.encode("utf-8")
    return struct.pack("<Q", len(encoded)) + encoded


def value_bytes(kind: int, value: Any) -> bytes:
    if kind == STRING:
        return string_bytes(value)
    if kind == ARRAY:
        element_kind, elements = value
        return struct.pack("<IQ", element_kind, len(elements)) + b"".join(
            value_bytes(element_kind, item) for item in elements
        )
    formats = {U32: "<I", I32: "<i", F32: "<f"}
    return struct.pack(formats[kind], u32(value) if kind == U32 else value)


@dataclass(frozen=True)
class Tensor:
    source: str
    name: str
    shape: tuple[int, ...]  # HF / NumPy order; GGML dimensions reverse this.

    @property
    def dtype(self) -> gguf.GGMLQuantizationType:
        if len(self.shape) == 1:
            return gguf.GGMLQuantizationType.F32
        # Preserve upstream policy: embeddings, heads, norms & biases stay float.
        if any(part in self.name for part in ("_embd", "codebook", "_norm", ".bias", "lm_head", "codec_head")):
            return gguf.GGMLQuantizationType.F16
        if self.shape[-1] % 32:
            raise ValueError(f"Q8_0 row width must be divisible by 32: {self.name}")
        return gguf.GGMLQuantizationType.Q8_0

    @property
    def nbytes(self) -> int:
        count = math.prod(self.shape)
        if self.dtype == gguf.GGMLQuantizationType.Q8_0:
            return count // 32 * 34  # FP16 scale + 32 signed bytes per block.
        return count * (4 if self.dtype == gguf.GGMLQuantizationType.F32 else 2)


def tensor_plan(config: dict[str, Any]) -> dict[str, list[Tensor]]:
    talker = config["talker_config"]
    predictor = talker["code_predictor_config"]
    h, p = talker["hidden_size"], predictor["hidden_size"]
    tv, cv = talker["text_vocab_size"], talker["vocab_size"]
    pv, text = predictor["vocab_size"], talker["text_hidden_size"]
    plans: dict[str, list[Tensor]] = {"talker": [], "code-predictor": []}

    def add(component: str, source: str, name: str, *shape: int) -> None:
        plans[component].append(Tensor(source, name, shape))

    for source, name, shape in (
        ("model.codec_embedding.weight", "codec_embd.weight", (cv, h)),
        ("model.text_embedding.weight", "text_embd.weight", (tv, text)),
        ("codec_head.weight", "codec_head.weight", (cv, h)),
        ("model.norm.weight", "output_norm.weight", (h,)),
        ("text_projection.linear_fc1.weight", "text_proj.fc1.weight", (h, text)),
        ("text_projection.linear_fc1.bias", "text_proj.fc1.bias", (h,)),
        ("text_projection.linear_fc2.weight", "text_proj.fc2.weight", (h, h)),
        ("text_projection.linear_fc2.bias", "text_proj.fc2.bias", (h,)),
    ):
        add("talker", f"talker.{source}", f"talker.{name}", *shape)

    add("code-predictor", "talker.code_predictor.model.norm.weight", "code_pred.output_norm.weight", p)
    for suffix, shape in (("weight", (p, h)), ("bias", (p,))):
        add("code-predictor", f"talker.code_predictor.small_to_mtp_projection.{suffix}",
            f"code_pred.small_to_mtp_projection.{suffix}", *shape)
    for index in range(talker["num_code_groups"] - 1):
        add("code-predictor", f"talker.code_predictor.model.codec_embedding.{index}.weight",
            f"code_pred.codec_embd.{index}.weight", pv, h)
        add("code-predictor", f"talker.code_predictor.lm_head.{index}.weight",
            f"code_pred.lm_head.{index}.weight", pv, p)

    for component, cfg, source_prefix, target_prefix in (
        ("talker", talker, "talker.model", "talker"),
        ("code-predictor", predictor, "talker.code_predictor.model", "code_pred"),
    ):
        hidden, intermediate, dim = cfg["hidden_size"], cfg["intermediate_size"], cfg["head_dim"]
        query, kv = cfg["num_attention_heads"] * dim, cfg["num_key_value_heads"] * dim
        layer_map = (
            ("input_layernorm", "attn_norm", (hidden,)),
            ("self_attn.q_proj", "attn_q", (query, hidden)),
            ("self_attn.k_proj", "attn_k", (kv, hidden)),
            ("self_attn.v_proj", "attn_v", (kv, hidden)),
            ("self_attn.o_proj", "attn_output", (hidden, query)),
            ("self_attn.q_norm", "attn_q_norm", (dim,)),
            ("self_attn.k_norm", "attn_k_norm", (dim,)),
            ("post_attention_layernorm", "ffn_norm", (hidden,)),
            ("mlp.gate_proj", "ffn_gate", (intermediate, hidden)),
            ("mlp.up_proj", "ffn_up", (intermediate, hidden)),
            ("mlp.down_proj", "ffn_down", (hidden, intermediate)),
        )
        for index in range(cfg["num_hidden_layers"]):
            for source, name, shape in layer_map:
                add(component, f"{source_prefix}.layers.{index}.{source}.weight",
                    f"{target_prefix}.blk.{index}.{name}.weight", *shape)
    return plans


class SafeWeights:
    """Read only validated float safetensors; decode BF16 without Torch."""

    def __init__(self, path: Path, plans: dict[str, list[Tensor]]):
        self.path = path
        size = path.stat().st_size
        with path.open("rb") as source:
            prefix = source.read(8)
            if len(prefix) != 8:
                raise ValueError("truncated safetensors length")
            header_size = struct.unpack("<Q", prefix)[0]
            if not 0 < header_size <= min(size - 8, 100_000_000):
                raise ValueError("invalid safetensors header length")
            self.header = json.loads(source.read(header_size))
        self.header.pop("__metadata__", None)
        self.data_start = 8 + header_size
        expected = {tensor.source for plan in plans.values() for tensor in plan}
        missing, extra = expected - self.header.keys(), self.header.keys() - expected
        if missing or extra:
            raise ValueError(f"checkpoint tensor mismatch: missing={sorted(missing)}, unmapped={sorted(extra)}")
        spans = []
        for plan in plans.values():
            for tensor in plan:
                entry = self.header[tensor.source]
                if tuple(entry["shape"]) != tensor.shape:
                    raise ValueError(f"{tensor.source}: expected {tensor.shape}, got {entry['shape']}")
                itemsize = {"BF16": 2, "F16": 2, "F32": 4}.get(entry["dtype"])
                if itemsize is None:
                    raise ValueError(f"unsupported dtype for {tensor.source}: {entry['dtype']}")
                start, end = entry["data_offsets"]
                if type(start) is not int or type(end) is not int or start < 0 or end - start != math.prod(tensor.shape) * itemsize:
                    raise ValueError(f"invalid offsets for {tensor.source}")
                spans.append((start, end))
                _ = tensor.dtype  # Validate quantizable row widths before output.
        cursor = 0
        for start, end in sorted(spans):
            if start != cursor:
                raise ValueError("overlapping or discontinuous safetensors data")
            cursor = end
        if cursor != size - self.data_start:
            raise ValueError("safetensors payload size mismatch")

    def write_tensor(self, output: BinaryIO, tensor: Tensor) -> None:
        entry = self.header[tensor.source]
        dtype = {"BF16": "<u2", "F16": "<f2", "F32": "<f4"}[entry["dtype"]]
        mapped = np.memmap(self.path, mode="r", dtype=dtype,
                           offset=self.data_start + entry["data_offsets"][0], shape=tensor.shape)
        rows = mapped.reshape(-1, tensor.shape[-1])
        chunk_rows = max(1, CHUNK_BYTES // (tensor.shape[-1] * 4))
        before = output.tell()
        try:
            for start in range(0, len(rows), chunk_rows):
                chunk = rows[start:start + chunk_rows]
                if entry["dtype"] == "BF16":
                    data = (chunk.astype("<u4") << 16).view("<f4")
                else:
                    data = chunk.astype("<f4")
                if not np.isfinite(data).all():
                    raise ValueError(f"non-finite weights in {tensor.source}")
                if tensor.dtype == gguf.GGMLQuantizationType.Q8_0:
                    encoded = gguf.quants.quantize(data, tensor.dtype)
                    # Quantizer returns byte blocks with little-endian FP16 scales.
                else:
                    encoded = data.astype("<f4" if tensor.dtype == gguf.GGMLQuantizationType.F32 else "<f2")
                    if not np.isfinite(encoded).all():
                        raise ValueError(f"float conversion overflow in {tensor.source}")
                output.write(encoded.tobytes(order="C"))
        finally:
            del rows, mapped
        if output.tell() - before != tensor.nbytes:
            raise ValueError(f"encoded byte count mismatch: {tensor.name}")


def model_metadata(config: dict[str, Any], mode: str, component: str) -> Metadata:
    talker = config["talker_config"]
    predictor = talker["code_predictor_config"]
    meta: Metadata = {
        "general.architecture": (STRING, ARCH),
        "general.name": (STRING, f"Qwen3-TTS-12Hz-1.7B-{mode}-{component}"),
        "general.type": (STRING, "model"),
        "general.alignment": (U32, ALIGNMENT),
        "general.file_type": (U32, int(gguf.LlamaFileType.MOSTLY_Q8_0)),
        "general.quantization_version": (U32, gguf.GGML_QUANT_VERSION),
        f"{ARCH}.tts_model_type": (STRING, config["tts_model_type"]),
        f"{ARCH}.tts_model_size": (STRING, "1.7b"),
        f"{ARCH}.text.vocab_size": (U32, talker["text_vocab_size"]),
        f"{ARCH}.text.embedding_dim": (U32, talker["text_hidden_size"]),
        f"{ARCH}.talker.codec_vocab_size": (U32, talker["vocab_size"]),
        f"{ARCH}.talker.num_codebooks": (U32, talker["num_code_groups"]),
    }
    # Keep talker metadata in both files; predictor loader reads its own file.
    for prefix, cfg in (("talker", talker), ("code_pred", predictor)):
        for suffix, source in (
            ("embedding_length", "hidden_size"), ("attention.key_length", "head_dim"),
            ("attention.value_length", "head_dim"), ("attention.head_count", "num_attention_heads"),
            ("attention.head_count_kv", "num_key_value_heads"),
            ("feed_forward_length", "intermediate_size"),
            ("block_count" if prefix == "talker" else "layer_count", "num_hidden_layers"),
            ("vocab_size", "vocab_size"),
        ):
            meta[f"{ARCH}.{prefix}.{suffix}"] = (U32, cfg[source])
        meta[f"{ARCH}.{prefix}.attention.layer_norm_rms_epsilon"] = (F32, cfg["rms_norm_eps"])
        meta[f"{ARCH}.{prefix}.rope.freq_base"] = (F32, cfg["rope_theta"])
    for suffix, source in (("pad_id", "codec_pad_id"), ("bos_id", "codec_bos_id"),
                           ("eos_id", "codec_eos_token_id"), ("think_id", "codec_think_id"),
                           ("nothink_id", "codec_nothink_id"), ("think_bos_id", "codec_think_bos_id"),
                           ("think_eos_id", "codec_think_eos_id")):
        meta[f"{ARCH}.codec.{suffix}"] = (U32, talker[source])
    for kind in ("bos", "eos", "pad"):
        key = f"tts_{kind}_token_id"
        meta[f"{ARCH}.{key}"] = (U32, config[key])
    languages = talker["codec_language_id"]
    for language, token_id in languages.items():
        meta[f"{ARCH}.talker.codec_language_id.{language}"] = (U32, token_id)
    meta[f"{ARCH}.language.english_id"] = (U32, languages["english"])
    meta[f"{ARCH}.rope.mrope_section"] = (ARRAY, (U32, talker["rope_scaling"]["mrope_section"]))
    if mode == "customvoice" and component == "talker":
        for speaker, token_id in talker["spk_id"].items():
            if not 0 <= u32(token_id) < talker["vocab_size"]:
                raise ValueError(f"speaker outside codec vocabulary: {speaker}")
            meta[f"{ARCH}.talker.spk_id.{speaker.lower()}"] = (U32, token_id)
            dialect = talker["spk_is_dialect"].get(speaker, False)
            if dialect is False:
                continue  # Loader consumes STRING dialects, never BOOL false.
            if not isinstance(dialect, str) or dialect not in languages:
                raise ValueError(f"invalid speaker dialect: {speaker}: {dialect!r}")
            meta[f"{ARCH}.talker.spk_is_dialect.{speaker.lower()}"] = (STRING, dialect)
    return meta


def tokenizer_metadata(model_dir: Path, config: dict[str, Any], mode: str) -> Metadata:
    cfg = load_json(model_dir / "tokenizer_config.json")
    vocab = load_json(model_dir / "vocab.json")
    size = config["talker_config"]["text_vocab_size"]
    tokens = [f"[PAD{index}]" for index in range(size)]
    types = [int(gguf.TokenType.UNUSED)] * size
    occupied: dict[int, str] = {}

    def insert(token: str, token_id: int, token_type: int) -> None:
        token_id = u32(token_id)
        if token_id >= size or (token_id in occupied and occupied[token_id] != token):
            raise ValueError(f"invalid/conflicting tokenizer ID: {token!r}: {token_id}")
        if token in vocab and vocab[token] != token_id:
            raise ValueError(f"conflicting tokenizer token: {token!r}")
        occupied[token_id] = token
        tokens[token_id], types[token_id] = token, token_type
        vocab[token] = token_id

    for token, token_id in list(vocab.items()):
        kind = gguf.TokenType.CONTROL if token.startswith("<|") and token.endswith("|>") else gguf.TokenType.NORMAL
        insert(token, token_id, int(kind))
    for token_id, entry in cfg["added_tokens_decoder"].items():
        insert(entry["content"], int(token_id), int(gguf.TokenType.CONTROL if entry.get("special") else gguf.TokenType.NORMAL))
    if not ("assistant" in vocab or "Ġassistant" in vocab) or not ("Ċ" in vocab or "\n" in vocab):
        raise ValueError("tokenizer lacks assistant/newline tokens required by QTS")

    def special_id(key: str) -> int:
        token = cfg[key]
        if isinstance(token, dict):
            token = token["content"]
        if token not in vocab:
            raise ValueError(f"missing tokenizer {key}: {token!r}")
        return vocab[token]

    bos, eos, pad = config["im_start_token_id"], special_id("eos_token"), special_id("pad_token")
    if occupied.get(bos) != "<|im_start|>" or eos != config["im_end_token_id"]:
        raise ValueError("tokenizer chat boundary IDs disagree with model config")
    for kind in ("bos", "eos", "pad"):
        if config[f"tts_{kind}_token_id"] not in occupied:
            raise ValueError(f"missing TTS {kind} token")
    merges = [line.strip() for line in (model_dir / "merges.txt").read_text(encoding="utf-8").splitlines()
              if line.strip() and not line.lstrip().startswith("#")]
    if not merges:
        raise ValueError("empty BPE merges")
    meta: Metadata = {
        "general.architecture": (STRING, ARCH),
        "general.name": (STRING, f"Qwen3-TTS-12Hz-1.7B-{mode}-tokenizer"),
        "general.type": (STRING, "model"),
        "general.alignment": (U32, ALIGNMENT),
        "tokenizer.ggml.model": (STRING, "gpt2"),
        "tokenizer.ggml.pre": (STRING, "qwen2"),
        "tokenizer.ggml.tokens": (ARRAY, (STRING, tokens)),
        "tokenizer.ggml.token_type": (ARRAY, (I32, types)),
        "tokenizer.ggml.merges": (ARRAY, (STRING, merges)),
        "tokenizer.ggml.bos_token_id": (U32, bos),
        "tokenizer.ggml.eos_token_id": (U32, eos),
        "tokenizer.ggml.padding_token_id": (U32, pad),
    }
    if cfg.get("chat_template"):
        meta["tokenizer.chat_template"] = (STRING, cfg["chat_template"])
    return meta


def write_gguf(path: Path, meta: Metadata, plan: list[Tensor], weights: SafeWeights) -> None:
    """Stream GGUF v3, little endian, without retaining converted model tensors."""
    with path.open("wb") as output:
        output.write(struct.pack("<4sIQQ", b"GGUF", 3, len(plan), len(meta)))
        for key, (kind, value) in meta.items():
            output.write(string_bytes(key) + struct.pack("<I", kind) + value_bytes(kind, value))
        offset = 0
        for tensor in plan:
            output.write(string_bytes(tensor.name) + struct.pack("<I", len(tensor.shape)))
            output.write(struct.pack("<" + "Q" * len(tensor.shape), *reversed(tensor.shape)))
            output.write(struct.pack("<IQ", int(tensor.dtype), offset))
            offset += tensor.nbytes + (-tensor.nbytes % ALIGNMENT)
        output.write(b"\0" * (-output.tell() % ALIGNMENT))
        for index, tensor in enumerate(plan, 1):
            print(f"{path.name}: {index}/{len(plan)} {tensor.name} {tensor.dtype.name}", flush=True)
            weights.write_tensor(output, tensor)
            output.write(b"\0" * (-tensor.nbytes % ALIGNMENT))


def main() -> None:
    if sys.byteorder != "little":
        raise ValueError("export requires little-endian host for upstream Q8_0 bytes")
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", type=Path, required=True, help="Local HF config/tokenizer directory")
    parser.add_argument("--weights", type=Path, required=True, help="Local model.safetensors")
    parser.add_argument("--mode", choices=("customvoice", "voicedesign"), required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--quant", choices=("q8_0",), required=True)
    args = parser.parse_args()
    model_dir, weights_path, out = (path.expanduser().resolve() for path in (args.model_dir, args.weights, args.out))
    config = load_json(model_dir / "config.json")
    expected_mode = {"customvoice": "custom_voice", "voicedesign": "voice_design"}[args.mode]
    if (config["tts_model_type"] != expected_mode or config["tts_model_size"] != "1b7"
            or config["tokenizer_type"] != "qwen3_tts_tokenizer_12hz"):
        raise ValueError("--mode requires matching official Qwen3-TTS 12Hz 1.7B config")
    talker = config["talker_config"]
    predictor = talker["code_predictor_config"]
    if (talker["hidden_size"], talker["num_hidden_layers"], predictor["hidden_size"], predictor["num_hidden_layers"]) != (2048, 28, 1024, 5):
        raise ValueError("unsupported 1.7B talker/predictor geometry")
    if bool(talker["spk_id"]) != (args.mode == "customvoice"):
        raise ValueError("CustomVoice requires presets; VoiceDesign must have none")
    plans = tensor_plan(config)
    weights = SafeWeights(weights_path, plans)
    metadata = {component: model_metadata(config, args.mode, component) for component in plans}
    metadata["tokenizer"] = tokenizer_metadata(model_dir, config, args.mode)
    plans["tokenizer"] = []
    stem = f"qwen3-tts-1.7b-{args.mode}"
    outputs = {component: out / (f"{stem}-tokenizer.gguf" if component == "tokenizer"
                                else f"{stem}-{component}-{args.quant}.gguf") for component in plans}
    for path in outputs.values():
        if path.exists():
            raise FileExistsError(f"refusing to overwrite existing export: {path}")
    out.mkdir(parents=True, exist_ok=True)
    staged: dict[str, Path] = {}
    try:
        for component, path in outputs.items():
            fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".partial", dir=out)
            os.close(fd)
            staged[component] = Path(temporary)
            write_gguf(staged[component], metadata[component], plans[component], weights)
        for component, path in outputs.items():
            os.replace(staged[component], path)
    finally:
        for path in staged.values():
            path.unlink(missing_ok=True)
    print("\nExport summary (shapes: HF / GGML):")
    for component, path in outputs.items():
        plan = plans[component]
        print(f"{path}: tensors={len(plan)}, types={dict(Counter(t.dtype.name for t in plan))}, "
              f"bytes={path.stat().st_size:,} ({path.stat().st_size / 2**20:.2f} MiB)")
        for tensor in plan:
            if "_embd" in tensor.name or "proj" in tensor.name:
                print(f"  {tensor.name}: {tensor.shape} / {tuple(reversed(tensor.shape))} {tensor.dtype.name}")
    token_meta = metadata["tokenizer"]
    print(f"tokenizer: tokens={len(token_meta['tokenizer.ggml.tokens'][1][1])}, "
          f"merges={len(token_meta['tokenizer.ggml.merges'][1][1])}")
    print(f"speakers={talker['spk_id'] if args.mode == 'customvoice' else {}}")
    print(f"vocoder: reuse {out / 'qwen3-tts-vocoder.onnx'} (not exported or modified)")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"export failed: {error}", file=sys.stderr)
        sys.exit(1)
