"""Build one group B precision variant from a source ONNX and verify it on CPU EP.

  py -3.11 make_variant.py --src <model.onnx> --out <dir> --id <variant id> --mode fp16|weights
      [--keep-io 0|1] [--outputs-fp32] [--block-op OP ...] [--block-name SUBSTR ...]
      [--block-prefix PREFIX ...] [--embed-table] [--source-label TEXT] [--notes TEXT]

  mode fp16    : onnxruntime.transformers.float16.convert_float_to_float16 (weights + activations)
  mode weights : weights stored fp16 with a Cast to fp32 per tensor; activations stay fp32
Writes <out>/model.onnx + model.onnx_data, optional embed_tokens.f16.npy, and manifest.json.
"""
import argparse
import sys
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper
from onnxruntime.transformers.float16 import DEFAULT_OP_BLOCK_LIST, convert_float_to_float16

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cfg  # noqa: E402  (scripts/)
from common import (FLOAT, FLOAT16, TABLE_NAME, describe, file_entry, measure,  # noqa: E402
                    save_model, status_of, write_manifest)


def to_fp16(model, args):
    block_nodes = [n.name for n in model.graph.node
                   if any(s in n.name for s in args.block_name)
                   or any(n.name.startswith(p) for p in args.block_prefix)]
    ops = list(DEFAULT_OP_BLOCK_LIST) + list(args.block_op)
    print(f"fp16: keep_io={bool(args.keep_io)} blocked_ops={args.block_op} blocked_nodes={len(block_nodes)}", flush=True)
    out = convert_float_to_float16(model, keep_io_types=bool(args.keep_io), op_block_list=ops,
                                   node_block_list=block_nodes or None, max_finite_val=65504.0)
    return out, len(block_nodes)


def dedupe_casts(model):
    """Converter emits one Cast per (tensor, target) per blocked consumer; keep the first, rewire the rest."""
    g = model.graph
    seen, rename, keep = {}, {}, []
    for n in g.node:
        if n.op_type == "Cast" and n.input:
            to = next((a.i for a in n.attribute if a.name == "to"), None)
            key = (n.input[0], to)
            if key in seen:
                rename[n.output[0]] = seen[key]
                continue
            seen[key] = n.output[0]
        keep.append(n)
    if not rename:
        return model, 0
    for n in keep:
        for i, x in enumerate(n.input):
            if x in rename:
                n.input[i] = rename[x]
    del g.node[:]
    g.node.extend(keep)
    return model, len(rename)


def to_weights_fp16(model):
    g = model.graph
    casts, kept, converted, overflow = [], [], 0, 0
    for init in g.initializer:
        arr = numpy_helper.to_array(init) if init.data_type == FLOAT else None
        if arr is None or arr.size < 1024:
            kept.append(init)
            continue
        overflow += int(np.count_nonzero(np.abs(arr) > 65504.0))
        h = arr.astype(np.float16)
        fname = init.name + "__f16"
        kept.append(numpy_helper.from_array(h, fname))
        casts.append(helper.make_node("Cast", [fname], [init.name], to=FLOAT, name="cast_w/" + init.name))
        converted += 1
    del g.initializer[:]
    g.initializer.extend(kept)
    existing = list(g.node)
    del g.node[:]
    g.node.extend(casts + existing)  # weight casts first keeps topological order
    return model, converted, overflow


def outputs_to_fp32(model):
    """Keep the original output names; consumers and producers use an internal fp16 tensor, then Cast."""
    g = model.graph
    for o in g.output:
        if o.type.tensor_type.elem_type != FLOAT16:
            continue
        inner = o.name + "__fp16"
        for n in g.node:
            for i, x in enumerate(n.input):
                if x == o.name:
                    n.input[i] = inner
            for i, x in enumerate(n.output):
                if x == o.name:
                    n.output[i] = inner
        g.node.append(helper.make_node("Cast", [inner], [o.name], to=FLOAT, name="cast_out/" + o.name))
        o.type.tensor_type.elem_type = FLOAT
    return model


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--tokenizer", required=True)
    ap.add_argument("--ref", required=True, help="FP32 CPU reference json (fp32-cpu-ref.json)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--id", required=True)
    ap.add_argument("--mode", choices=["fp16", "weights"], required=True)
    ap.add_argument("--keep-io", type=int, default=1)
    ap.add_argument("--outputs-fp32", action="store_true")
    ap.add_argument("--block-op", action="append", default=[])
    ap.add_argument("--block-name", action="append", default=[])
    ap.add_argument("--block-prefix", action="append", default=[])
    ap.add_argument("--embed-table", action="store_true")
    ap.add_argument("--source-label", default="")
    ap.add_argument("--notes", default="")
    args = ap.parse_args()

    cfg.set_paths(tokenizer=args.tokenizer, ref=args.ref)
    src, out = Path(args.src), Path(args.out)
    print("loading", src, flush=True)
    model = onnx.load(str(src), load_external_data=True)
    notes = [args.notes] if args.notes else []
    conv = {"mode": args.mode}

    if args.mode == "fp16":
        model, nblock = to_fp16(model, args)
        model, ndup = dedupe_casts(model)
        conv.update({"keep_io_types": bool(args.keep_io), "blocked_ops": args.block_op,
                     "blocked_name_substrings": args.block_name, "blocked_name_prefixes": args.block_prefix,
                     "blocked_nodes": nblock, "duplicate_casts_removed": ndup,
                     "min_positive_val": 5.96e-08, "max_finite_val": 65504.0})
        notes.append("epsilon constants below 5.96e-08 are clamped to the fp16 subnormal floor by the converter")
    else:
        model, converted, overflow = to_weights_fp16(model)
        conv.update({"weights_converted_fp16": converted, "weights_out_of_fp16_range": overflow,
                     "min_tensor_elems": 1024, "keep_io_types": True})
        notes.append("weights rounded to fp16 at rest; activations and io stay fp32")

    if args.outputs_fp32:
        model = outputs_to_fp32(model)
        conv["outputs_fp32"] = True

    path = save_model(model, out)
    del model
    desc = describe(path)
    files = {"model.onnx": file_entry(path), "model.onnx_data": file_entry(out / "model.onnx_data")}

    table = None
    if args.embed_table:
        base = onnx.load(str(src), load_external_data=True)
        init = next(i for i in base.graph.initializer if i.name == "model.embed_tokens.weight")
        arr = numpy_helper.to_array(init)
        table16 = arr.astype(np.float16)
        np.save(out / TABLE_NAME, table16)
        files[TABLE_NAME] = file_entry(out / TABLE_NAME)
        notes.append("embed_tokens.f16.npy is the fp32 base table cast to fp16; lookup is table[input_ids]")
        conv["embed_table_overflow_values"] = int(np.count_nonzero(np.abs(arr) > 65504.0))
        table = np.load(out / TABLE_NAME)
        del base

    print("measuring on CPU EP", flush=True)
    result = measure(path, table=table)
    status = status_of(result)
    runs = result["runs"]
    if status == "FAILED":
        notes.append("CPU EP errors: " + "; ".join(r["error"][:300] for r in runs.values() if "error" in r))
    manifest = {
        "id": args.id,
        "source": {"file": Path(src).name, "label": args.source_label},
        "conversion_options": conv,
        "node_count": desc["node_count"],
        "op_histogram": desc["op_histogram"],
        "cast_nodes": desc["cast_nodes"],
        "inputs": desc["inputs"],
        "outputs": desc["outputs"],
        "files": files,
        "bytes_total": sum(v["bytes"] for v in files.values()),
        "cpu_cosine_vs_ref": {k: runs[k].get("cosine") for k in ("32", "128", "512")},
        "ids_match_ref": {k: runs[k].get("ids_match_ref") for k in ("32", "128", "512")},
        "finite": all(runs[k].get("finite", False) for k in ("32", "128", "512")),
        "cpu_p50_ms_128_20runs": runs["128"].get("p50_ms_128"),
        "status": status,
        "checks": runs,
        "notes": notes,
        "cpu_execution_provider_only": True,
    }
    write_manifest(out, manifest)
    print(f"{args.id} status={status} cos32={manifest['cpu_cosine_vs_ref']['32']} "
          f"cos128={manifest['cpu_cosine_vs_ref']['128']} cos512={manifest['cpu_cosine_vs_ref']['512']} "
          f"bytes={manifest['bytes_total']}", flush=True)


if __name__ == "__main__":
    main()
