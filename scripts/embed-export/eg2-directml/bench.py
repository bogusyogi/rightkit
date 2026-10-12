"""EmbeddingGemma 2 execution-provider benchmark.

Fixed-shape, batch-1 latency per execution provider, with node placement,
adapter, memory and cosine against the FP32 CPU reference.

  py -3.11 bench.py --model <model.onnx> --tokenizer <tokenizer.json> --provider cpu|cuda|tensorrt|dml
                    --tag <name> --work-dir <folder> [--fp16-trt] [--ref <cpu result json>] [--device-id N]

Needs psutil; the cuda and tensorrt providers also import torch for its CUDA DLLs. Runs nvidia-smi and
writes trt-cache/, profiles/ and results/ under --work-dir.
"""
import argparse
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

HERE = Path(__file__).resolve().parent


def work_dir(args):
    """Folder for trt-cache/, profiles/ and results/ (--work-dir); created if missing."""
    w = Path(args.work_dir).resolve()
    w.mkdir(parents=True, exist_ok=True)
    return w
LENGTHS = [32, 64, 128, 256, 512]
DOC_PREFIX = "title: none | text: "
QUERY_PREFIX = "task: search result | query: "
# One fixed passage, truncated to each length; recorded verbatim in the output.
PASSAGE = (
    "The build broker serialises compilation requests from every agent on the machine so that "
    "two workspaces never fight over the same target directory. Each request names its "
    "repository, the cargo arguments and a deadline; the broker admits it when capacity "
    "allows, records a receipt, and streams the compiler output back to the caller. When the "
    "queue is long the broker prefers short checks over full release builds, because a "
    "developer waiting on a type error is blocked in a way that a nightly packaging job is "
    "not. Failed requests keep their logs for a day so the cause can be read after the fact. "
) * 12
QUERY = "how does the build broker decide which compilation request runs first"
WARMUP, ITERS = 20, 100


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def gpu_mem_mib():
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=name,memory.used,driver_version", "--format=csv,noheader,nounits"],
            capture_output=True, text=True, timeout=20, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        ).stdout.strip().splitlines()[0]
        name, used, driver = [p.strip() for p in out.split(",")]
        return {"name": name, "used_mib": int(used), "driver": driver}
    except Exception as exc:  # recorded, never fatal
        return {"error": str(exc)}


def rss_mib():
    try:
        import psutil
        info = psutil.Process().memory_info()
        return {"rss_mib": info.rss / 2**20, "peak_wset_mib": getattr(info, "peak_wset", 0) / 2**20}
    except Exception as exc:
        return {"error": str(exc)}


def providers_for(args):
    if args.provider == "cpu":
        return ["CPUExecutionProvider"]
    if args.provider == "cuda":
        return [("CUDAExecutionProvider", {"device_id": args.device_id}), "CPUExecutionProvider"]
    if args.provider == "tensorrt":
        cache = work_dir(args) / "trt-cache" / args.tag
        cache.mkdir(parents=True, exist_ok=True)
        return [
            ("TensorrtExecutionProvider", {
                "device_id": args.device_id,
                "trt_fp16_enable": bool(args.fp16_trt),
                "trt_engine_cache_enable": True,
                "trt_engine_cache_path": str(cache),
            }),
            ("CUDAExecutionProvider", {"device_id": args.device_id}),
            "CPUExecutionProvider",
        ]
    if args.provider == "dml":
        return [("DmlExecutionProvider", {"device_id": args.device_id}), "CPUExecutionProvider"]
    raise SystemExit(f"unknown provider {args.provider}")


def placement(profile_path):
    """Count executed nodes per provider from an ORT profile."""
    counts, cpu_ops = {}, {}
    seen = set()
    for ev in json.load(open(profile_path, encoding="utf-8")):
        a = ev.get("args") or {}
        if ev.get("cat") != "Node" or not ev.get("name", "").endswith("_kernel_time"):
            continue
        node = ev["name"][: -len("_kernel_time")]
        if node in seen:
            continue
        seen.add(node)
        prov = a.get("provider", "?")
        counts[prov] = counts.get(prov, 0) + 1
        if prov == "CPUExecutionProvider":
            cpu_ops[a.get("op_name", "?")] = cpu_ops.get(a.get("op_name", "?"), 0) + 1
    return {"nodes_by_provider": counts, "cpu_ops": cpu_ops, "nodes_seen": len(seen)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--tokenizer", required=True, help="tokenizer.json of the text-only model")
    ap.add_argument("--work-dir", required=True, help="folder for trt-cache/, profiles/ and results/")
    ap.add_argument("--provider", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--device-id", type=int, default=0)
    ap.add_argument("--fp16-trt", action="store_true")
    ap.add_argument("--ref")
    ap.add_argument("--lengths", default=",".join(map(str, LENGTHS)))
    ap.add_argument("--warmup", type=int, default=WARMUP)
    ap.add_argument("--iters", type=int, default=ITERS)
    ap.add_argument("--no-profile", action="store_true")
    args = ap.parse_args()

    if args.provider in ("cuda", "tensorrt"):
        import torch  # noqa: F401  (its bundled CUDA 12 / cuDNN 9 DLLs let the ORT CUDA provider load)
    # "{L}" in --model selects a static per-length file (model_{L}.onnx).
    lengths = [int(x) for x in args.lengths.split(",")]
    model_for = lambda L: Path(args.model.replace("{L}", str(L)))
    model = model_for(lengths[0])
    data = model.with_name(model.name + "_data")
    if not data.exists():
        data = model.with_name("model.onnx_data")
    tables = sorted(model.parent.glob("embed_tokens*.npy"))
    table = np.load(tables[0], mmap_mode="r") if tables else None
    tok = Tokenizer.from_file(args.tokenizer)
    tok.no_padding()
    tok.no_truncation()
    doc_ids = tok.encode(DOC_PREFIX + PASSAGE).ids
    query_ids = tok.encode(QUERY_PREFIX + QUERY).ids
    ref = json.load(open(args.ref, encoding="utf-8")) if args.ref else None

    out = {
        "tag": args.tag, "provider": args.provider, "device_id": args.device_id,
        "fp16_trt": bool(args.fp16_trt),
        "onnxruntime": ort.__version__, "available_providers": ort.get_available_providers(),
        "python": sys.version.split()[0],
        "model_files": {model_for(L).name: {"sha256": sha256(model_for(L)), "bytes": model_for(L).stat().st_size}
                        for L in lengths},
        "tokenizer_sha256": sha256(args.tokenizer),
        "doc_prefix": DOC_PREFIX, "query_prefix": QUERY_PREFIX,
        "passage": PASSAGE, "query": QUERY, "query_token_count": len(query_ids),
        "pooling_and_l2_norm": "in graph (output sentence_embedding)",
        "timing_scope": "session.run with numpy inputs and outputs, batch 1; includes host<->device copies, excludes tokenisation",
        "warmup": args.warmup, "iters": args.iters, "gpu_before": gpu_mem_mib(), "cpu_percent_before_3s": __import__("psutil").cpu_percent(interval=3), "runs": {},
    }
    if data.exists():
        out["model_files"][data.name] = {"sha256": sha256(data), "bytes": data.stat().st_size}

    if table is not None:
        out["model_files"][tables[0].name] = {"bytes": tables[0].stat().st_size}
        out["embed_lookup"] = "host side, unscaled table rows, outside timing"
    for length in lengths:
        model = model_for(length)
        ids = np.array([doc_ids[:length]], dtype=np.int64)
        assert ids.shape[1] == length, "passage too short"
        feed = {"input_ids": ids, "attention_mask": np.ones_like(ids)}
        so = ort.SessionOptions()
        so.add_free_dimension_override_by_name("batch_size", 1)
        so.add_free_dimension_override_by_name("sequence_length", length)
        so.enable_profiling = not args.no_profile
        so.profile_file_prefix = str(work_dir(args) / "profiles" / f"{args.tag}-{length}")
        (work_dir(args) / "profiles").mkdir(parents=True, exist_ok=True)
        run = {"token_ids_sha256": hashlib.sha256(ids.tobytes()).hexdigest(), "tokens": length}
        try:
            t0 = time.perf_counter()
            sess = ort.InferenceSession(str(model), so, providers=providers_for(args))
            run["load_s"] = time.perf_counter() - t0
            run["session_providers"] = sess.get_providers()
            run["provider_options"] = {k: dict(v) for k, v in sess.get_provider_options().items()}
            ins = {i.name: i.type for i in sess.get_inputs()}
            if "inputs_embeds" in ins:
                dt = np.float16 if "float16" in ins["inputs_embeds"] else np.float32
                feed = {"inputs_embeds": np.ascontiguousarray(table[ids[0]][None], dtype=dt), "attention_mask": feed["attention_mask"]}
            feed = {k: v for k, v in feed.items() if k in ins}
            t0 = time.perf_counter()
            first = sess.run(["sentence_embedding"], feed)[0]
            run["first_run_s"] = time.perf_counter() - t0
            for _ in range(args.warmup):
                sess.run(["sentence_embedding"], feed)
            times = []
            for _ in range(args.iters):
                t = time.perf_counter()
                vec = sess.run(["sentence_embedding"], feed)[0]
                times.append((time.perf_counter() - t) * 1000)
            times.sort()
            run["p50_ms"] = statistics.median(times)
            run["p95_ms"] = times[int(0.95 * len(times)) - 1]
            run["min_ms"], run["max_ms"] = times[0], times[-1]
            vec = vec[0].astype(np.float64)
            run["norm"] = float(np.linalg.norm(vec))
            run["finite"] = bool(np.isfinite(vec).all())
            run["vector"] = [float(x) for x in vec]
            if ref:
                r = np.array(ref["runs"][str(length)]["vector"])
                run["cosine_vs_ref"] = float(vec @ r / (np.linalg.norm(vec) * np.linalg.norm(r)))
            run["memory_after"] = rss_mib()
            run["gpu_after"] = gpu_mem_mib()
            if not args.no_profile:
                run["placement"] = placement(sess.end_profiling())
        except Exception as exc:
            run["error"] = f"{type(exc).__name__}: {exc}"[:2000]
        out["runs"][str(length)] = run
        brief = {k: run.get(k) for k in ("load_s", "p50_ms", "p95_ms", "cosine_vs_ref", "error")}
        brief["placement"] = (run.get("placement") or {}).get("nodes_by_provider")
        print(length, json.dumps(brief), flush=True)
        sess = None

    dest = work_dir(args) / "results" / f"{args.tag}.json"
    dest.parent.mkdir(parents=True, exist_ok=True)
    json.dump(out, open(dest, "w", encoding="utf-8"), indent=1)
    print("wrote", dest)


if __name__ == "__main__":
    main()
