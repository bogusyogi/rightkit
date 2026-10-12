"""Build Group E ONNX variants of EmbeddingGemma 2 (text-only) on top of group D, verified on CPU.

  py -3.11 scripts\\groupE\\build_e.py --variant E40-fp16-prescale-gelu1-foldgamma-rope4-embeds-fp32tail ^
      --src <model_text.onnx> --tokenizer <tokenizer.json> --ref <fp32-cpu-ref.json> --out <export folder>
  (all paths are required arguments; --variant all builds every variant in VARIANTS)

Reuses scripts\\groupD\\build_d.py (verify, convert_fp16, describe_model, file_entry, constants) and
scripts\\groupD\\passes_d.py. Writes exports\\<id>\\model_{32,64,128,256,512}.onnx sharing model.onnx_data,
embed_tokens.f16.npy (fp16 of the source embedding table) for host-lookup variants, and manifest.json.
Status OK = finite and cosine >= 0.999 at 32/128/512; otherwise DEGRADED (files kept).
Runs at BELOW_NORMAL process priority (Windows) so the build does not starve other work on the machine.
"""
import argparse
import copy
import json
import os
import shutil
import sys
import time
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.dirname(os.path.dirname(HERE))
GROUP_D = os.path.join(WORK, "scripts", "groupD")
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))  # scripts/ (cfg)
sys.path.insert(0, GROUP_D)
import cfg  # noqa: E402  tokenizer and reference come from the entry point arguments

import numpy as np  # noqa: E402
import onnx  # noqa: E402

import build_d as B  # noqa: E402  (groupD: verify, convert_fp16, describe_model, file_entry, constants)
import passes_d as D  # noqa: E402
import passes_e as E  # noqa: E402
import passes as P  # noqa: E402  (groupA)

DATA_NAME = B.DATA_NAME
TABLE_NAME = B.TABLE_NAME
LENGTHS = B.LENGTHS
THRESHOLD = B.THRESHOLD

_BASE_ALL = ["gelu1", "rope", "lpnorm-prescale"]
VARIANTS = {
    "E33-fp16-prescale-gelu1-embeds": {
        "base": _BASE_ALL + ["embeds"], "fold": False, "rope4": False, "fp16": True, "table": True,
        "notes": "D29 with each GELU as ONE ai.onnx Gelu node (approximate='none', erf form), as D30 does. "
                 "Norm: u = x * (1/64) -> LpNormalization(p=2) -> Mul(gamma*sqrt(D)). Token lookup on host, fp16 "
                 "inputs_embeds. Table = fp16 cast of the source embedding initializer. Each static length: CSE, DCE, topo order and "
                 "no-op pair removal after every pass; fp16 keep_io_types=0, outputs fp32.",
    },
    "E34-fp16-prescale-gelu1-foldgamma-embeds": {
        "base": _BASE_ALL + ["embeds", "fold"], "fold": True, "rope4": False, "fp16": True, "table": True,
        "notes": "E33 plus constant folding in fp32 before fp16: a constant scale Mul/Div directly before a "
                 "constant-weight MatMul is folded into its weight (W' = diag(c) W, duplicated per consumer), and "
                 "the same for a scale directly after a MatMul (W' = W diag(c)). The norm output Mul(gamma*sqrt(D)) "
                 "is removed where its consumers are weight MatMuls. Table = fp16 cast of the source embedding initializer.",
    },
    "E35-fp16-prescale-gelu1-foldgamma-rope4-embeds": {
        "base": _BASE_ALL + ["embeds", "fold"], "fold": True, "rope4": True, "fp16": True, "table": True,
        "notes": "E34 plus a compact RoPE: each rotate-half subgraph becomes out = Mul(x, COS) + "
                 "Mul(Gather(x, PERM, axis=-1), SIN), 4 nodes. COS = concat(cos,cos) tiled over heads, "
                 "SIN = concat(-sin,sin) tiled over heads, PERM swaps the two halves of each head. All constants "
                 "precomputed per static length and deduplicated by content (shared across layers). "
                 "Table = fp16 cast of the source embedding initializer.",
    },
    "E36-fp16-prescale-gelu1-foldgamma-rope4": {
        "base": _BASE_ALL + ["fold"], "fold": True, "rope4": True, "fp16": True, "table": False,
        "notes": "E35 with the token lookup kept in the graph: input_ids int64 [1,L] and attention_mask int64 "
                 "[1,L] are the inputs; the fp32 embedding table is inside the graph and converted to fp16 by "
                 "the converter.",
    },
}


def lower_priority():
    """BELOW_NORMAL_PRIORITY_CLASS on Windows; best effort, never fatal."""
    try:
        import ctypes
        k32 = ctypes.windll.kernel32
        k32.SetPriorityClass(k32.GetCurrentProcess(), 0x00004000)
    except Exception:
        pass


def apply_base_e(base, passes, side, stats):
    hits = {}
    for p in passes:
        if p == "gelu1":
            hits[p] = D.gelu_none(base)
        elif p == "rope":
            hits[p] = P.rope(base)
        elif p == "lpnorm-prescale":
            hits[p], st = D.lp_norm(base, "prescale")
            hits["lpnorm_scalar_scales"] = st["scalar_scales"]
            hits["lpnorm_scale_fp16_overflow"] = st["scale_const_overflow_fp16"]
        elif p == "embeds":
            hits[p] = P.embeds(base, side)
        elif p == "fold":
            hits["fold"] = E.fold_scales(base)
        else:
            raise SystemExit(f"unknown pass {p}")
    hits["clean_base"] = E.clean(base)
    return hits


def build_one(base, L, spec):
    """static(L) -> rope4 -> clean -> merge -> fp16 -> clean. Returns (model, info)."""
    info = {}
    m, folded = P.static(base, L)
    info["static_folded_nodes"] = folded
    if spec["rope4"]:
        matched, unmatched = E.rope_compact(m)
        info["rope4_matched"] = matched
        info["rope4_unmatched"] = unmatched
    info["clean_1"] = E.clean(m)
    info["const_scale_merges"] = D.merge_const_scale(m)
    info["clean_2"] = E.clean(m)
    P.drop_unused_domains(m)
    info["ndup_casts"] = 0
    if spec["fp16"]:
        m, ndup = B.convert_fp16(m)
        info["ndup_casts"] = ndup
        info["clean_fp16"] = E.clean(m)
    info["op_histogram"] = B.op_hist(m)
    info["node_count"] = len(m.graph.node)
    info["cos_sin_left"] = sum(1 for n in m.graph.node if n.op_type in ("Cos", "Sin"))
    return m, info


def build_and_write(src, vid, spec, out_dir, overwrite=False):
    t0 = time.perf_counter()
    base = copy.deepcopy(src)
    side = {}
    base_hits = apply_base_e(base, spec["base"], side, None)
    if spec["table"] and "embed_table" not in side:
        raise SystemExit("embeds pass did not produce a table")

    B.check_out_dir(out_dir, overwrite)
    if os.path.isdir(out_dir):
        shutil.rmtree(out_dir)
    os.makedirs(out_dir)
    data_path = os.path.join(out_dir, DATA_NAME)
    writer = D.SharedWriter(data_path)
    per, first, onnx_paths = {}, None, {}
    for L in LENGTHS:
        m, info = build_one(base, L, spec)
        per[str(L)] = {"node_count": info["node_count"], "static_folded_nodes": info["static_folded_nodes"],
                       "const_scale_merges": info["const_scale_merges"],
                       "duplicate_casts_removed": info["ndup_casts"],
                       "rope_cos_sin_nodes_left": info["cos_sin_left"],
                       "rope4_matched": info.get("rope4_matched"), "rope4_unmatched": info.get("rope4_unmatched"),
                       "clean_stats": {"1": info["clean_1"], "2": info["clean_2"],
                                       "fp16": info.get("clean_fp16")},
                       "op_histogram": info["op_histogram"]}
        path = os.path.join(out_dir, f"model_{L}.onnx")
        writer.save(m, path)
        onnx_paths[L] = path
        if first is None:
            first = B.describe_model(m)
        print(vid, "L", L, "nodes", info["node_count"], "folded", info["static_folded_nodes"],
              "rope4", info.get("rope4_matched"), "unmatched", info.get("rope4_unmatched"),
              "cos_sin_left", info["cos_sin_left"], flush=True)
        del m
    total_data = writer.close()
    del base

    table_entry = None
    if spec["table"]:
        table_dst = os.path.join(out_dir, TABLE_NAME)
        np.save(table_dst, side["embed_table"].astype(np.float16))
        table_entry = B.file_entry(table_dst)

    files = {f"model_{L}.onnx": B.file_entry(onnx_paths[L]) for L in LENGTHS}
    files[DATA_NAME] = B.file_entry(data_path)
    if table_entry:
        files[TABLE_NAME] = table_entry
    return {"per": per, "first": first, "base_hits": base_hits, "files": files, "onnx_paths": onnx_paths,
            "build_seconds": round(time.perf_counter() - t0, 1), "data_bytes": total_data}


def main():
    lower_priority()
    ap = argparse.ArgumentParser()
    ap.add_argument("--variant", required=True, help="a key of VARIANTS or 'all'")
    ap.add_argument("--src", required=True, help="FP32 text-only graph model_text.onnx")
    ap.add_argument("--tokenizer", required=True, help="tokenizer.json of the text-only model")
    ap.add_argument("--ref", required=True, help="FP32 CPU reference json (fp32-cpu-ref.json)")
    ap.add_argument("--out", required=True, help="output folder for exports (one subfolder per variant)")
    ap.add_argument("--no-verify", action="store_true")
    ap.add_argument("--overwrite", action="store_true", help="replace an existing <out>/<variant> folder")
    args = ap.parse_args()
    cfg.set_paths(tokenizer=args.tokenizer, ref=args.ref)

    ids = list(VARIANTS) if args.variant == "all" else [args.variant]
    for vid in ids:
        if vid not in VARIANTS:
            raise SystemExit(f"unknown variant {vid}")

    for vid in ids:
        B.check_out_dir(os.path.join(args.out, vid), args.overwrite)
    t0 = time.perf_counter()
    src = onnx.load(args.src, load_external_data=True)
    print("loaded source in", round(time.perf_counter() - t0, 1), "s", flush=True)
    src_info = {"file": os.path.basename(args.src), "sha256_model": B.sha256_file(args.src),
                "bytes_model": os.path.getsize(args.src), "bytes_data": os.path.getsize(args.src + "_data")}

    for vid in ids:
        spec = VARIANTS[vid]
        out_dir = os.path.join(args.out, vid)
        info = build_and_write(src, vid, spec, out_dir, overwrite=args.overwrite)
        status, results = (None, None)
        if not args.no_verify:
            status, results = B.verify(out_dir, info["onnx_paths"], spec["table"])
        files = info["files"]
        cos = {k: results[k].get("cosine_vs_ref") for k in map(str, LENGTHS)} if results else None
        passes = list(spec["base"]) + ["static(" + ",".join(map(str, LENGTHS)) + ")"]
        if spec["rope4"]:
            passes.append("rope_compact(4 nodes)")
        passes += ["clean(cse,dce,topo,noop)", "const_scale_merge"]
        if spec["fp16"]:
            passes.append("fp16(keep_io_types=0, outputs fp32)")
        manifest = {
            "id": vid,
            "source": src_info,
            "passes_applied": passes,
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
