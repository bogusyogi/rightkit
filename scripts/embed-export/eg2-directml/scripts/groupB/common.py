"""Shared helpers for group B fp16 precision variants (CPU EP only)."""
import collections
import hashlib
import json
import os
import statistics
import sys
import time
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, numpy_helper
from tokenizers import Tokenizer

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))  # folder root that holds bench.py
sys.path.insert(0, os.path.dirname(HERE))  # scripts/ (cfg)
sys.path.insert(0, ROOT)
import cfg  # noqa: E402  tokenizer and reference come from the entry point's arguments
from bench import DOC_PREFIX, PASSAGE  # noqa: E402  exact prompt/passage as bench.py

CHECK_LENGTHS = [32, 128, 512]
TABLE_NAME = "embed_tokens.f16.npy"

FLOAT, FLOAT16 = TensorProto.FLOAT, TensorProto.FLOAT16
DTYPE_NAMES = {TensorProto.FLOAT: "float32", TensorProto.FLOAT16: "float16",
               TensorProto.INT64: "int64", TensorProto.INT32: "int32"}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def file_entry(path):
    path = Path(path)
    return {"sha256": sha256(path), "bytes": path.stat().st_size}


def doc_ids(length):
    tok = Tokenizer.from_file(cfg.path("tokenizer"))
    tok.no_padding()
    tok.no_truncation()
    return tok.encode(DOC_PREFIX + PASSAGE).ids[:length]


def save_model(model, out_dir):
    """Write out_dir/model.onnx with weights in out_dir/model.onnx_data (bench.py naming)."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    for stale in ("model.onnx", "model.onnx_data"):
        p = out_dir / stale
        if p.exists():
            p.unlink()
    onnx.save_model(model, str(out_dir / "model.onnx"), save_as_external_data=True,
                    all_tensors_to_one_file=True, location="model.onnx_data", size_threshold=1024)
    return out_dir / "model.onnx"


def describe(path):
    """Node count, op histogram, cast count and io dtypes, read without weights."""
    m = onnx.load(str(path), load_external_data=False)
    g = m.graph
    hist = collections.Counter((n.domain or "ai.onnx") + ":" + n.op_type for n in g.node)
    dt = lambda vi: DTYPE_NAMES.get(vi.type.tensor_type.elem_type, str(vi.type.tensor_type.elem_type))
    return {
        "node_count": len(g.node),
        "op_histogram": dict(hist.most_common()),
        "cast_nodes": hist.get("ai.onnx:Cast", 0),
        "inputs": {i.name: dt(i) for i in g.input},
        "outputs": {o.name: dt(o) for o in g.output},
        "opsets": {o.domain or "ai.onnx": o.version for o in m.opset_import},
    }


def load_table(out_dir):
    """Token table saved next to a variant (fp16); None when absent."""
    p = Path(out_dir) / TABLE_NAME
    return np.load(p) if p.exists() else None


def build_feed(sess, ids, table):
    feed = {"attention_mask": np.ones_like(ids)}
    types = {i.name: i.type for i in sess.get_inputs()}
    if "input_ids" in types:
        feed["input_ids"] = ids
    if "inputs_embeds" in types:
        if table is None:
            raise RuntimeError("inputs_embeds model needs " + TABLE_NAME)
        dtype = np.float16 if "float16" in types["inputs_embeds"] else np.float32
        feed["inputs_embeds"] = table[ids].astype(dtype)
    return feed


def measure(model_path, table=None, timing_len=128, warmup=3, timing_runs=20):
    """CPU EP only. Cosine vs the FP32 CPU reference at each check length, finiteness, p50 at 128."""
    ref = json.load(open(cfg.path("ref"), encoding="utf-8"))["runs"]
    sess = ort.InferenceSession(str(model_path), ort.SessionOptions(),
                                providers=["CPUExecutionProvider"])
    out = {"runs": {}, "session_providers": sess.get_providers()}
    for length in CHECK_LENGTHS:
        ids = np.array([doc_ids(length)], dtype=np.int64)
        run = {"token_ids_sha256": hashlib.sha256(ids.tobytes()).hexdigest()}
        try:
            feed = build_feed(sess, ids, table)
            vec = sess.run(["sentence_embedding"], feed)[0][0].astype(np.float64)
            r = np.array(ref[str(length)]["vector"])
            run["finite"] = bool(np.isfinite(vec).all())
            run["norm"] = float(np.linalg.norm(vec)) if run["finite"] else None
            run["cosine"] = float(vec @ r / (np.linalg.norm(vec) * np.linalg.norm(r))) if run["finite"] else float("nan")
            run["ids_match_ref"] = run["token_ids_sha256"] == ref[str(length)]["token_ids_sha256"]
            if length == timing_len:
                for _ in range(warmup):
                    sess.run(["sentence_embedding"], feed)
                times = []
                for _ in range(timing_runs):
                    t = time.perf_counter()
                    sess.run(["sentence_embedding"], feed)
                    times.append((time.perf_counter() - t) * 1000)
                run["p50_ms_128"] = statistics.median(times)
        except Exception as exc:  # recorded as FAILED, never hidden
            run["error"] = f"{type(exc).__name__}: {exc}"[:1500]
        out["runs"][str(length)] = run
    return out


def status_of(result):
    runs = result["runs"]
    if any("error" in r for r in runs.values()):
        return "FAILED"
    if not all(r["finite"] for r in runs.values()):
        return "NAN"
    if min(r["cosine"] for r in runs.values()) < 0.999:
        return "DEGRADED"
    return "OK"


def write_manifest(out_dir, manifest):
    p = Path(out_dir) / "manifest.json"
    p.write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    return p
