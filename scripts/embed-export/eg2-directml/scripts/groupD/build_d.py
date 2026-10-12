"""Build Group D fewer-node ONNX variants of EmbeddingGemma 2 (text-only) and verify them on CPU.

  py -3.11 scripts\\groupD\\build_d.py --variant D26-fp32-static-lpnorm
  py -3.11 scripts\\groupD\\build_d.py --variant all

Writes exports\\<id>\\model_{32,64,128,256,512}.onnx sharing model.onnx_data, embed_tokens.f16.npy
(fp16 of the source embedding table) for the host-embedding variants, and manifest.json. Each length is verified on
CPUExecutionProvider against results\\fp32-cpu-ref.json (cosine vs ref at 32/128/512).
Status OK = finite and cosine >= 0.999 at 32/128/512; otherwise DEGRADED (files kept).
"""
import argparse
import copy
import hashlib
import json
import os
import shutil
import sys
import time
from collections import Counter

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto
from onnxruntime.transformers.float16 import DEFAULT_OP_BLOCK_LIST, convert_float_to_float16

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))  # scripts/ (cfg)
import cfg  # noqa: E402  tokenizer and reference come from the entry point arguments
sys.path.insert(0, os.path.join(WORK, "scripts", "groupA"))
sys.path.insert(0, os.path.join(WORK, "scripts", "groupB"))
import passes as P  # noqa: E402  (groupA)
import verify as V  # noqa: E402  (groupA: token_ids with the bench.py prompt and hash check)
import passes_d as D  # noqa: E402
from make_variant import dedupe_casts, outputs_to_fp32  # noqa: E402  (groupB)

DATA_NAME = "model.onnx_data"
TABLE_NAME = "embed_tokens.f16.npy"
LENGTHS = [32, 64, 128, 256, 512]
CHECK = [32, 128, 512]
THRESHOLD = 0.999

# base passes run once on the dynamic source; then static(L) per length; fp16 after that.
VARIANTS = {
    "D26-fp32-static-lpnorm": {
        "base": ["gelu", "rope", "lpnorm"], "fp16": False, "table": False,
        "notes": "A11 pipeline (tanh Gelu decomposed, RoPE standard ops, static per length) with every RMS norm "
                 "replaced by LpNormalization(axis=-1,p=2) + one Mul by gamma*sqrt(D). eps is DROPPED (the "
                 "LpNormalization form has none); the source eps values are 1e-6 for the layer norms and 5.12e-4 "
                 "for per_layer_projection_norm. A zero input row would give NaN (no floor).",
    },
    "D27-fp16-static-stable-norm-embeds": {
        "base": ["gelu", "rope", "stable-norm", "embeds"], "fp16": True, "table": True,
        "notes": "B15 pipeline made static per length: max-abs stabilised RMS norm (passes.stable_norm), token "
                 "lookup on host, fp16 inputs_embeds, fp32 outputs via a final Cast. "
                 "Table = fp16 cast of the source embedding initializer (byte-identical to B15 for E40). Converter: ai.onnx op block list default, keep_io_types=False.",
    },
    "D28-fp16-static-lpnorm-maxabs-embeds": {
        "base": ["gelu", "rope", "lpnorm-maxabs", "embeds"], "fp16": True, "table": True,
        "notes": "fp16, static, host embeds. Norm: u = x / ReduceMax(Abs(x), -1) -> LpNormalization -> Mul(gamma*sqrt(D)). "
                 "No eps and no max floor: a zero row gives NaN. Table = fp16 cast of the source embedding initializer.",
    },
    "D29-fp16-static-lpnorm-prescale-embeds": {
        "base": ["gelu", "rope", "lpnorm-prescale", "embeds"], "fp16": True, "table": True,
        "notes": "fp16, static, host embeds. Norm: u = x * (1/64) -> LpNormalization -> Mul(gamma*sqrt(D)). "
                 "The max-abs division is replaced by the constant pre-scale. Scale-invariance keeps the value; "
                 "fp16 range is only extended to |x| up to about 590 in the squared sum. No eps.",
    },
    "D30-fp16-static-lpnorm-maxabs-gelu1-embeds": {
        "base": ["gelu1", "rope", "lpnorm-maxabs", "embeds"], "fp16": True, "table": True,
        "notes": "D28 with each GELU as ONE ai.onnx Gelu node, approximate='none' (erf form). The source GELU is "
                 "tanh; numerics change slightly, see the recorded cosines. Table = fp16 cast of the source embedding initializer.",
    },
    "D31-fp16-static-lpnorm-maxabs-gelu1": {
        "base": ["gelu1", "rope", "lpnorm-maxabs"], "fp16": True, "table": False,
        "notes": "D30 with the token lookup kept in the graph: input_ids int64 [1,L] and attention_mask int64 "
                 "[1,L] are the inputs; the fp32 embedding table is inside the graph and converted to fp16 "
                 "by the converter.",
    },
    "D32-fp32-static-lpnorm-gelu1": {
        "base": ["gelu1", "rope", "lpnorm"], "fp16": False, "table": False,
        "notes": "D26 with each GELU as ONE ai.onnx Gelu node (approximate='none', erf form). Numerics change "
                 "slightly relative to the tanh source; see the recorded cosines. eps dropped as in D26.",
    },
}


def op_hist(m):
    return dict(sorted(Counter(f"{n.domain or 'ai.onnx'}::{n.op_type}" for n in m.graph.node).items()))


def io_desc(vi):
    t = vi.type.tensor_type
    dims = [d.dim_value if d.HasField("dim_value") else (d.dim_param or "?") for d in t.shape.dim]
    return {"name": vi.name, "dtype": TensorProto.DataType.Name(t.elem_type), "shape": dims}


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 22), b""):
            h.update(chunk)
    return h.hexdigest()


def file_entry(path):
    return {"sha256": sha256_file(path), "bytes": os.path.getsize(path)}


def apply_base(base, passes, side):
    hits = {}
    for p in passes:
        if p == "gelu":
            hits[p] = P.gelu(base)
        elif p == "gelu1":
            hits[p] = D.gelu_none(base)
        elif p == "rope":
            hits[p] = P.rope(base)
        elif p == "lpnorm":
            hits[p], st = D.lp_norm(base, "plain")
            hits["lpnorm_scalar_scales"] = st["scalar_scales"]
            hits["lpnorm_scale_fp16_overflow"] = st["scale_const_overflow_fp16"]
        elif p == "lpnorm-maxabs":
            hits[p], st = D.lp_norm(base, "maxabs")
            hits["lpnorm_scalar_scales"] = st["scalar_scales"]
            hits["lpnorm_scale_fp16_overflow"] = st["scale_const_overflow_fp16"]
        elif p == "lpnorm-prescale":
            hits[p], st = D.lp_norm(base, "prescale")
            hits["lpnorm_scalar_scales"] = st["scalar_scales"]
            hits["lpnorm_scale_fp16_overflow"] = st["scale_const_overflow_fp16"]
        elif p == "stable-norm":
            hits[p] = P.stable_norm(base)
        elif p == "embeds":
            hits[p] = P.embeds(base, side)
        else:
            raise SystemExit(f"unknown pass {p}")
    hits["cse_removed_base"] = P.cse(base)
    P.dce(base)
    return hits


def convert_fp16(m):
    """Same options as groupB make_variant (keep_io_types=False, outputs fp32)."""
    m = convert_float_to_float16(m, keep_io_types=False, op_block_list=list(DEFAULT_OP_BLOCK_LIST),
                                 node_block_list=None, max_finite_val=65504.0)
    m, ndup = dedupe_casts(m)
    m = outputs_to_fp32(m)
    D.topo_sort(m)  # converter output is not topologically ordered; groupA dce needs it
    return m, ndup


def describe_model(m):
    return {
        "node_count": len(m.graph.node),
        "op_histogram": op_hist(m),
        "inputs": [io_desc(v) for v in m.graph.input],
        "outputs": [io_desc(v) for v in m.graph.output],
        "domains_opsets": {(o.domain or "ai.onnx"): o.version for o in m.opset_import},
    }


def check_out_dir(out_dir, overwrite):
    """Refuse to replace an existing export folder unless overwrite is set."""
    if os.path.isdir(out_dir) and not overwrite:
        raise SystemExit(f"refusing to overwrite existing export folder: {out_dir} (pass --overwrite to replace it)")


def build_and_write(src, vid, spec, out_dir, overwrite=False):
    t0 = time.perf_counter()
    base = copy.deepcopy(src)
    side = {}
    base_hits = apply_base(base, spec["base"], side)
    if spec["table"] and "embed_table" not in side:
        raise SystemExit("embeds pass did not produce a table")

    check_out_dir(out_dir, overwrite)
    if os.path.isdir(out_dir):
        shutil.rmtree(out_dir)
    os.makedirs(out_dir)
    data_path = os.path.join(out_dir, DATA_NAME)
    writer = D.SharedWriter(data_path)
    per, first = {}, None
    onnx_paths = {}
    for L in LENGTHS:
        m, folded = P.static(base, L)
        scale_merges = D.merge_const_scale(m)
        P.cse(m)
        P.dce(m)
        P.drop_unused_domains(m)
        ndup = 0
        if spec["fp16"]:
            m, ndup = convert_fp16(m)
            P.cse(m)
            P.dce(m)
        cos_rope = sum(1 for n in m.graph.node if n.op_type in ("Cos", "Sin"))
        per[str(L)] = {"node_count": len(m.graph.node), "static_folded_nodes": folded,
                       "const_scale_merges": scale_merges, "duplicate_casts_removed": ndup,
                       "rope_cos_sin_nodes_left": cos_rope, "op_histogram": op_hist(m)}
        path = os.path.join(out_dir, f"model_{L}.onnx")
        writer.save(m, path)
        onnx_paths[L] = path
        if first is None:
            first = describe_model(m)
        print(vid, "L", L, "nodes", per[str(L)]["node_count"], "folded", folded,
              "scale_merges", scale_merges, "casts_dedup", ndup, "cos_sin_left", cos_rope, flush=True)
        del m
    total_data = writer.close()
    del base

    table_entry = None
    if spec["table"]:
        table_dst = os.path.join(out_dir, TABLE_NAME)
        np.save(table_dst, side["embed_table"].astype(np.float16))
        table_entry = file_entry(table_dst)

    files = {f"model_{L}.onnx": file_entry(onnx_paths[L]) for L in LENGTHS}
    files[DATA_NAME] = file_entry(data_path)
    if table_entry:
        files[TABLE_NAME] = table_entry
    return {
        "per": per, "first": first, "base_hits": base_hits, "files": files,
        "onnx_paths": onnx_paths, "data_path": data_path, "build_seconds": round(time.perf_counter() - t0, 1),
        "data_bytes": total_data,
    }


def verify(out_dir, onnx_paths, has_table):
    ref = json.load(open(cfg.path("ref"), encoding="utf-8"))
    ids_by_len = V.token_ids(ref)
    table = np.load(os.path.join(out_dir, TABLE_NAME)) if has_table else None
    results = {}
    for L in LENGTHS:
        so = ort.SessionOptions()
        so.log_severity_level = 3
        try:
            sess = ort.InferenceSession(onnx_paths[L], so, providers=["CPUExecutionProvider"])
            types = {i.name: i.type for i in sess.get_inputs()}
            ids = ids_by_len[L]
            feed = {"attention_mask": np.ones_like(ids)}
            if "input_ids" in types:
                feed["input_ids"] = ids
            if "inputs_embeds" in types:
                if table is None:
                    raise RuntimeError("inputs_embeds model needs " + TABLE_NAME)
                dt = np.float16 if "float16" in types["inputs_embeds"] else np.float32
                feed["inputs_embeds"] = table[ids].astype(dt)
            vec = sess.run(["sentence_embedding"], feed)[0][0].astype(np.float64)
            r = np.array(ref["runs"][str(L)]["vector"], dtype=np.float64)
            finite = bool(np.isfinite(vec).all())
            cos = float(vec @ r / (np.linalg.norm(vec) * np.linalg.norm(r))) if finite else float("nan")
            results[str(L)] = {"cosine_vs_ref": cos, "finite": finite,
                               "max_abs_diff": float(np.max(np.abs(vec - r))) if finite else None,
                               "providers": sess.get_providers()}
            del sess
        except Exception as exc:
            results[str(L)] = {"error": f"{type(exc).__name__}: {exc}"[:1200], "finite": False,
                               "cosine_vs_ref": None}
        print("verify", L, {k: v for k, v in results[str(L)].items() if k != "providers"}, flush=True)
    status = "OK"
    for L in CHECK:
        r = results[str(L)]
        if "error" in r or not r["finite"] or r["cosine_vs_ref"] < THRESHOLD:
            status = "DEGRADED"
    if any("error" in results[str(L)] for L in CHECK):
        status = "FAILED"
    return status, results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--variant", required=True, help="a key of VARIANTS or 'all'")
    ap.add_argument("--src", required=True)
    ap.add_argument("--tokenizer", required=True, help="tokenizer.json of the text-only model")
    ap.add_argument("--ref", required=True, help="FP32 CPU reference json (fp32-cpu-ref.json)")
    ap.add_argument("--out", required=True, help="output folder for exports")
    ap.add_argument("--no-verify", action="store_true")
    ap.add_argument("--overwrite", action="store_true", help="replace an existing <out>/<variant> folder")
    args = ap.parse_args()
    cfg.set_paths(tokenizer=args.tokenizer, ref=args.ref)

    ids = list(VARIANTS) if args.variant == "all" else [args.variant]
    for vid in ids:
        if vid not in VARIANTS:
            raise SystemExit(f"unknown variant {vid}")

    for vid in ids:
        check_out_dir(os.path.join(args.out, vid), args.overwrite)
    t0 = time.perf_counter()
    src = onnx.load(args.src, load_external_data=True)
    print("loaded source in", round(time.perf_counter() - t0, 1), "s", flush=True)
    src_info = {"file": os.path.basename(args.src), "sha256_model": sha256_file(args.src),
                "bytes_model": os.path.getsize(args.src), "bytes_data": os.path.getsize(args.src + "_data")}

    for vid in ids:
        spec = VARIANTS[vid]
        out_dir = os.path.join(args.out, vid)
        info = build_and_write(src, vid, spec, out_dir, overwrite=args.overwrite)
        status, results = (None, None)
        if not args.no_verify:
            status, results = verify(out_dir, info["onnx_paths"], spec["table"])
        files = info["files"]
        cos = {k: results[k].get("cosine_vs_ref") for k in map(str, LENGTHS)} if results else None
        manifest = {
            "id": vid,
            "source": src_info,
            "passes_applied": spec["base"] + ["static(" + ",".join(map(str, LENGTHS)) + ")", "cse", "dce"]
                              + (["fp16(keep_io_types=0, outputs fp32)"] if spec["fp16"] else [])
                              + ["const_scale_merge"],
            "base_pass_hits": info["base_hits"],
            "node_count": {str(L): info["per"][str(L)]["node_count"] for L in LENGTHS},
            "per_length": info["per"],
            "op_histogram": info["per"]["128"]["op_histogram"],
            "domains_opsets": info["first"]["domains_opsets"],
            "inputs": info["first"]["inputs"],
            "outputs": info["first"]["outputs"],
            "pooling_and_l2_norm": "in graph",
            "files": files,
            "bytes_total": sum(v["bytes"] for v in files.values()),
            "cpu_cosine_vs_ref": cos,
            "cpu_max_abs_diff": {k: results[k].get("max_abs_diff") for k in map(str, LENGTHS)} if results else None,
            "cpu_finite": {k: results[k].get("finite") for k in map(str, LENGTHS)} if results else None,
            "threshold_cosine": THRESHOLD,
            "status": status,
            "cpu_execution_provider_only": True,
            "notes": spec["notes"],
            "build_seconds": info["build_seconds"],
        }
        with open(os.path.join(out_dir, "manifest.json"), "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2)
        print(vid, "status", status, "nodes128", manifest["node_count"]["128"], "bytes",
              manifest["bytes_total"], "cos", cos, flush=True)
    print("done", flush=True)


if __name__ == "__main__":
    main()
