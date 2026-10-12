"""Build the Group E eps variants (E37, E38) of EmbeddingGemma 2 (text-only), verified on CPU.

  py -3.11 scripts\\groupE\\build_eps.py --variant E37-fp16-prescale-epsadd-gelu1-foldgamma-rope4-embeds ^
      --src <model_text.onnx> --tokenizer <tokenizer.json> --ref <fp32-cpu-ref.json> --out <export folder> ^
      [--e35-manifest <E35 manifest.json>]
  (all paths are required arguments except --e35-manifest; --variant all builds every eps variant)

Reuses scripts\\groupE\\build_e.py (build_and_write, apply_base_e, B = groupD build_d for verify, file_entry,
constants) and passes_e.py. The norm rewrite is passes_eps.lp_norm_eps, applied to a copy of the source
before build_e's per-length pipeline; build_e's base passes then run unchanged (gelu1, rope, embeds, fold).
Writes exports\\<id>\\model_{32,64,128,256,512}.onnx sharing model.onnx_data, embed_tokens.f16.npy (derived
from the source embedding table), and manifest.json with the same fields as E35 plus "zero_row_check" and "eps_table".
Existing variants are not touched.
"""
import argparse
import copy
import json
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))  # scripts/ (cfg)
import cfg  # noqa: E402  tokenizer and reference come from the entry point arguments

import onnx  # noqa: E402

import build_e as BE  # noqa: E402  (groupE; also sets sys.path for groupA, groupB, groupD)
import passes_d as D  # noqa: E402  (groupD)
import passes_eps as PE  # noqa: E402
import zero_check as ZC  # noqa: E402

B = BE.B
E35_ID = "E35-fp16-prescale-gelu1-foldgamma-rope4-embeds"
BASE = ["gelu1", "rope", "embeds", "fold"]  # lpnorm is applied by passes_eps before these
VARIANTS = {
    "E37-fp16-prescale-epsadd-gelu1-foldgamma-rope4-embeds": {
        "norm_mode": "add", "pool_floor": True, "base": BASE, "fold": True, "rope4": True, "fp16": True,
        "table": True,
        "notes": "E35 with the RMS norm made zero-row safe. Norm: u = x * (1/64) -> n = ReduceL2(u, -1, keepdims) "
                 "-> d = n + eps_c -> y = u / d -> Mul(gamma*sqrt(D)). eps_c = fp16(sqrt(D*eps_src)/64) per norm "
                 "(eps_src from the source epsilon attribute of that norm node). Same table as E35 otherwise. "
                 "Sentence-embedding L2 floor 1e-12 replaced by 2^-20 (fp16-exact). Table = fp16 cast of the source embedding initializer.",
    },
    "E38-fp16-prescale-epsmax-gelu1-foldgamma-rope4-embeds": {
        "norm_mode": "max", "pool_floor": True, "base": BASE, "fold": True, "rope4": True, "fp16": True,
        "table": True,
        "notes": "E37 with d = Max(n, eps_c) instead of n + eps_c. For n >= eps_c the value equals the un-guarded "
                 "norm exactly; eps_c only acts on rows whose norm is below it. Same pre-scale, fold, RoPE and "
                 "table as E35. Sentence-embedding L2 floor 1e-12 replaced by 2^-20 (fp16-exact).",
    },
}
VARIANTS["E40-fp16-prescale-gelu1-foldgamma-rope4-embeds-fp32tail"] = {
    "norm_mode": "lpnorm", "pool_floor": False, "tail32": True, "base": BASE, "fold": True, "rope4": True,
    "fp16": True, "table": True,
    "notes": "E35 exactly (LpNormalization norms, no per-norm epsilon), except the sentence tail runs in fp32: "
             "mean16 -> Cast fp32 -> ReduceL2 -> Max(n, 1e-12) -> Div, output written directly as fp32 "
             "(final Cast removed). Pooling count floor Max(count, 1.0) in fp16 replaces the Cast/Max/Cast chain. "
             "Table = fp16 cast of the source embedding initializer.",
}

_orig_build_one = BE.build_one


def _build_one_tail(base, L, spec):
    m, info = _orig_build_one(base, L, spec)
    if spec.get("tail32"):
        info["tail32"] = PE.tail_fp32(m)
        info["node_count"] = len(m.graph.node)
        info["op_histogram"] = B.op_hist(m)
    return m, info


BE.build_one = _build_one_tail  # only variants with tail32 change; others pass through unchanged


def e35_nodes(path):
    """Node counts of the E35 export for the comparison field; None when no manifest is given."""
    if not path:
        return None
    with open(path, encoding="utf-8") as f:
        return json.load(f)["node_count"]


def zero_check(out_dir):
    res = {}
    for L in (32, 128):
        path = os.path.join(out_dir, f"model_{L}.onnx")
        try:
            r = ZC.check_one(path)
        except Exception as exc:
            r = {"error": f"{type(exc).__name__}: {exc}"[:800]}
        res[str(L)] = r
    ok = all(v.get("sentence_finite") is True for v in res.values())
    res["all_sentence_finite"] = ok
    res["provider"] = "CPUExecutionProvider"
    res["note"] = ("CPU only. ORT CPU LpNormalization of a zero vector already returns zeros, so E35 also "
                   "passes here; this check does not show the GPU (DML fp16) behaviour.")
    return res


def main():
    BE.lower_priority()
    ap = argparse.ArgumentParser()
    ap.add_argument("--variant", required=True, help="a key of VARIANTS or 'all'")
    ap.add_argument("--src", required=True, help="FP32 text-only graph model_text.onnx")
    ap.add_argument("--tokenizer", required=True, help="tokenizer.json of the text-only model")
    ap.add_argument("--ref", required=True, help="FP32 CPU reference json (fp32-cpu-ref.json)")
    ap.add_argument("--out", required=True, help="output folder for exports (one subfolder per variant)")
    ap.add_argument("--e35-manifest", default=None, help="optional E35 manifest.json for node_count_vs_E35")
    ap.add_argument("--no-verify", action="store_true")
    ap.add_argument("--overwrite", action="store_true", help="replace an existing <out>/<variant> folder")
    args = ap.parse_args()
    cfg.set_paths(tokenizer=args.tokenizer, ref=args.ref)

    ids = list(VARIANTS) if args.variant == "all" else [args.variant]
    for vid in ids:
        if vid not in VARIANTS:
            raise SystemExit(f"unknown variant {vid}")

    for vid in ids:
        BE.B.check_out_dir(os.path.join(args.out, vid), args.overwrite)
    t0 = time.perf_counter()
    src = onnx.load(args.src, load_external_data=True)
    print("loaded source in", round(time.perf_counter() - t0, 1), "s", flush=True)
    src_info = {"file": os.path.basename(args.src), "sha256_model": B.sha256_file(args.src),
                "bytes_model": os.path.getsize(args.src), "bytes_data": os.path.getsize(args.src + "_data")}
    e35_nc = e35_nodes(args.e35_manifest)

    for vid in ids:
        spec = VARIANTS[vid]
        out_dir = os.path.join(args.out, vid)
        s2 = copy.deepcopy(src)
        if spec["norm_mode"] == "lpnorm":
            norm_hits, _st = D.lp_norm(s2, "prescale")  # E35's norm, unchanged
            eps_table = {}
        else:
            norm_hits, eps_table = PE.lp_norm_eps(s2, spec["norm_mode"])
        floor_hits = PE.pool_floor(s2) if spec["pool_floor"] else 0
        print(vid, "norm rewrites", norm_hits, "eps table", eps_table, "pool floor", floor_hits, flush=True)
        build_spec = {k: spec[k] for k in ("base", "fold", "rope4", "fp16", "table")}
        build_spec["tail32"] = bool(spec.get("tail32", False))
        info = BE.build_and_write(s2, vid, build_spec, out_dir, overwrite=args.overwrite)
        del s2
        status, results = (None, None)
        if not args.no_verify:
            status, results = B.verify(out_dir, info["onnx_paths"], spec["table"])
        zc = zero_check(out_dir)
        files = info["files"]
        cos = {k: results[k].get("cosine_vs_ref") for k in map(str, B.LENGTHS)} if results else None
        norm_label = "lpnorm-prescale" if spec["norm_mode"] == "lpnorm" else "lpnorm-eps(" + spec["norm_mode"] + ")"
        passes = [norm_label, "gelu1", "rope", "embeds", "fold",
                  "static(" + ",".join(map(str, B.LENGTHS)) + ")", "rope_compact(4 nodes)",
                  "clean(cse,dce,topo,noop)", "const_scale_merge", "fp16(keep_io_types=0, outputs fp32)"]
        if spec.get("tail32"):
            passes.append("tail_fp32")
        node_count = {str(L): info["per"][str(L)]["node_count"] for L in B.LENGTHS}
        manifest = {
            "id": vid,
            "source": src_info,
            "passes_applied": passes,
            "base_pass_hits": {"lpnorm_eps_rewrites": norm_hits, "pool_floor_replaced": floor_hits,
                               **info["base_hits"]},
            "eps_table": eps_table,
            "pool_floor": {"old": PE.POOL_FLOOR_OLD, "new": PE.POOL_FLOOR_NEW},
            "node_count": node_count,
            "node_count_vs_E35": ({k: node_count[k] - e35_nc[k] for k in node_count} if e35_nc else None),
            "per_length": info["per"],
            "op_histogram": info["per"]["128"]["op_histogram"],
            "domains_opsets": info["first"]["domains_opsets"],
            "inputs": info["first"]["inputs"],
            "outputs": info["first"]["outputs"],
            "pooling_and_l2_norm": "in graph",
            "files": files,
            "bytes_total": sum(v["bytes"] for v in files.values()),
            "cpu_cosine_vs_ref": cos,
            "cpu_max_abs_diff": {k: results[k].get("max_abs_diff") for k in map(str, B.LENGTHS)} if results else None,
            "cpu_finite": {k: results[k].get("finite") for k in map(str, B.LENGTHS)} if results else None,
            "zero_row_check": zc,
            "threshold_cosine": B.THRESHOLD,
            "status": status,
            "cpu_execution_provider_only": True,
            "notes": spec["notes"],
            "build_seconds": info["build_seconds"],
        }
        with open(os.path.join(out_dir, "manifest.json"), "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2)
        print(vid, "status", status, "nodes128", node_count["128"], "vsE35", (manifest["node_count_vs_E35"] or {}).get("128"),
              "bytes", manifest["bytes_total"], "cos", cos, "zero_finite", zc["all_sentence_finite"], flush=True)
    print("done", flush=True)


if __name__ == "__main__":
    main()
