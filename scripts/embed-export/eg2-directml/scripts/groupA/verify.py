"""Verify an export variant on the CPU execution provider against fp32-cpu-ref.json.

  py -3.11 scripts\\groupA\\verify.py --dir exports\\<id>            (updates manifest.json)
  py -3.11 scripts\\groupA\\verify.py --model <model.onnx> --no-manifest   (baseline, e.g. the source)

Token ids come from bench.py's own PASSAGE/prefixes (imported, not copied); each length's
ids must hash to the reference's token_ids_sha256 or the run aborts. Timing: 3 warmups, then
20 timed runs at length 128 (p50). Only CPUExecutionProvider is ever requested.
"""
import argparse
import hashlib
import json
import os
import statistics
import sys

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, WORK)
sys.path.insert(0, os.path.dirname(HERE))  # scripts/ (cfg)
import cfg  # noqa: E402  tokenizer and reference come from the entry point arguments
import bench  # noqa: E402  (constants only; its main() is guarded)

LENGTHS = [32, 64, 128, 256, 512]
THRESHOLD = 0.99999


def token_ids(ref):
    tok = Tokenizer.from_file(cfg.path("tokenizer"))
    tok.no_padding()
    tok.no_truncation()
    doc = tok.encode(bench.DOC_PREFIX + bench.PASSAGE).ids
    out = {}
    for L in LENGTHS:
        ids = np.array([doc[:L]], dtype=np.int64)
        got = hashlib.sha256(ids.tobytes()).hexdigest()
        want = ref["runs"][str(L)]["token_ids_sha256"]
        if got != want:
            raise SystemExit(f"token ids mismatch at length {L}: {got} != {want}")
        out[L] = ids
    return out


def make_session(path):
    so = ort.SessionOptions()
    so.log_severity_level = 3  # errors only; shape-merge warnings are not results
    return ort.InferenceSession(path, so, providers=["CPUExecutionProvider"])


def feeds_for(sess, ids, table):
    mask = np.ones_like(ids)
    names = [i.name for i in sess.get_inputs()]
    if "inputs_embeds" in names:
        return {"inputs_embeds": np.asarray(table[ids], dtype=np.float32), "attention_mask": mask}
    return {"input_ids": ids, "attention_mask": mask}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", help="exports\\<id> folder with manifest.json")
    ap.add_argument("--model", help="single model path (baseline mode)")
    ap.add_argument("--no-manifest", action="store_true")
    ap.add_argument("--timing-length", type=int, default=128)
    ap.add_argument("--tokenizer", required=True, help="tokenizer.json of the text-only model")
    ap.add_argument("--ref", required=True, help="FP32 CPU reference json (fp32-cpu-ref.json)")
    args = ap.parse_args()
    cfg.set_paths(tokenizer=args.tokenizer, ref=args.ref)

    ref = json.load(open(cfg.path("ref"), encoding="utf-8"))
    ids_by_len = token_ids(ref)

    static_files = None
    manifest_path = None
    if args.dir:
        manifest_path = os.path.join(args.dir, "manifest.json")
        manifest = json.load(open(manifest_path, encoding="utf-8"))
        # Host-lookup variants ship an fp16 table; older fp32-table variants are still accepted.
        table_path = os.path.join(args.dir, "embed_tokens.f16.npy")
        if not os.path.exists(table_path):
            table_path = os.path.join(args.dir, "embed_tokens.f32.npy")
        if os.path.exists(os.path.join(args.dir, "model.onnx")):
            model_for = {L: os.path.join(args.dir, "model.onnx") for L in LENGTHS}
        else:
            model_for = {L: os.path.join(args.dir, f"model_{L}.onnx") for L in LENGTHS}
            static_files = True
    else:
        manifest = None
        model_for = {L: args.model for L in LENGTHS}
        table_path = None

    table = None
    results = {}
    for L in LENGTHS:
        path = model_for[L]
        sess = make_session(path)
        feeds = feeds_for(sess, ids_by_len[L], _load_table(table_path, sess))
        vec = sess.run(["sentence_embedding"], feeds)[0][0].astype(np.float64)
        r = np.array(ref["runs"][str(L)]["vector"], dtype=np.float64)
        cos = float(vec @ r / (np.linalg.norm(vec) * np.linalg.norm(r)))
        entry = {"cosine_vs_ref": cos, "max_abs_diff": float(np.max(np.abs(vec - r))),
                 "finite": bool(np.isfinite(vec).all()), "file": os.path.basename(path)}
        if L == args.timing_length:
            for _ in range(3):
                sess.run(["sentence_embedding"], feeds)
            times = []
            import time
            for _ in range(20):
                t0 = time.perf_counter()
                sess.run(["sentence_embedding"], feeds)
                times.append((time.perf_counter() - t0) * 1000)
            entry["p50_ms"] = statistics.median(times)
            entry["runs"] = len(times)
        results[L] = entry
        print(L, json.dumps({k: v for k, v in entry.items() if k != "file"}), flush=True)
        del sess

    key = [32, 128, 512]
    cmin = min(results[L]["cosine_vs_ref"] for L in key)
    all_finite = all(results[L]["finite"] for L in LENGTHS)
    status = "OK" if (cmin >= THRESHOLD and all_finite) else "FAILED"
    print(f"cosine_min_32_128_512={cmin:.9f} status={status}")

    if manifest is not None and not args.no_manifest:
        manifest["cpu_cosine_vs_ref"] = {str(L): results[L]["cosine_vs_ref"] for L in LENGTHS}
        manifest["cpu_max_abs_diff"] = {str(L): results[L]["max_abs_diff"] for L in LENGTHS}
        manifest["cpu_cosine_min_32_128_512"] = cmin
        manifest["cpu_p50_ms_128"] = results[args.timing_length]["p50_ms"]
        manifest["cpu_timing"] = {"length": args.timing_length, "warmup": 3, "runs": 20,
                                  "provider": "CPUExecutionProvider", "onnxruntime": ort.__version__}
        manifest["status"] = status
        manifest["threshold_cosine"] = THRESHOLD
        json.dump(manifest, open(manifest_path, "w", encoding="utf-8"), indent=2)
        print("updated", manifest_path)


def _load_table(table_path, sess):
    if table_path and "inputs_embeds" in [i.name for i in sess.get_inputs()]:
        return np.load(table_path, mmap_mode="r")
    return None


if __name__ == "__main__":
    main()
