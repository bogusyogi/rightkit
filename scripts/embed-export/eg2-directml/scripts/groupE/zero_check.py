"""Zero-row and masked-row checks for an export folder (CPU by default, DirectML on request).

  py -3.11 scripts\\groupE\\zero_check.py --dir <export folder> [--lengths 32,128]
  py -3.11 scripts\\groupE\\zero_check.py --dir <export folder> --provider dml [--device-id 0]

Per length, two cases (inputs_embeds models only; in-graph input_ids models are rejected):
  (a) inputs_embeds = 0 with attention_mask = 1 (every row is a zero embedding)
  (b) inputs_embeds = embed_tokens.f16.npy rows for token ids 2..L+1 with attention_mask = 0
For each case: finite yes/no (sentence_embedding and last_hidden_state) and max abs of the finite values.
Exit status 1 if any case is not finite or errors. --provider dml requires DmlExecutionProvider to be
active in the session. Writes nothing.

check_one(path) keeps the original CPU case (a) only; build_eps.py imports it for the manifest field.
"""
import argparse
import json
import os
import sys

import numpy as np
import onnxruntime as ort

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE_NAME = "embed_tokens.f16.npy"


def check_one(path):
    so = ort.SessionOptions()
    so.log_severity_level = 3
    sess = ort.InferenceSession(path, so, providers=["CPUExecutionProvider"])
    types = {i.name: i.type for i in sess.get_inputs()}
    L = int(os.path.basename(path).split("_")[-1].split(".")[0])
    dt = np.float16 if "float16" in types["inputs_embeds"] else np.float32
    feed = {"inputs_embeds": np.zeros((1, L, 512), dtype=dt), "attention_mask": np.ones((1, L), dtype=np.int64)}
    sent, hid = sess.run(["sentence_embedding", "last_hidden_state"], feed)
    sent = sent.astype(np.float64)[0]
    hid = hid.astype(np.float64)[0]
    out = {
        "length": L,
        "sentence_finite": bool(np.isfinite(sent).all()),
        "sentence_nan": int(np.isnan(sent).sum()),
        "sentence_inf": int(np.isinf(sent).sum()),
        "sentence_max_abs": float(np.max(np.abs(sent))),
        "sentence_first8": [float(v) for v in sent[:8]],
        "last_hidden_finite": bool(np.isfinite(hid).all()),
        "last_hidden_nan": int(np.isnan(hid).sum()),
        "last_hidden_max_abs": float(np.max(np.abs(hid))),
    }
    return out


def open_session(path, provider, device_id):
    so = ort.SessionOptions()
    so.log_severity_level = 3
    if provider == "cpu":
        sess = ort.InferenceSession(path, so, providers=["CPUExecutionProvider"])
    else:
        sess = ort.InferenceSession(path, so, providers=[("DmlExecutionProvider", {"device_id": device_id})])
        if "DmlExecutionProvider" not in sess.get_providers():
            raise SystemExit(f"DmlExecutionProvider is not active; session providers = {sess.get_providers()}")
    return sess


def finite_max(x):
    vals = x[np.isfinite(x)]
    return float(np.max(np.abs(vals))) if vals.size else None


def run_case(sess, feed):
    sent, hid = sess.run(["sentence_embedding", "last_hidden_state"], feed)
    sent = sent.astype(np.float64)
    hid = hid.astype(np.float64)
    s_fin = bool(np.isfinite(sent).all())
    h_fin = bool(np.isfinite(hid).all())
    return {
        "finite": s_fin and h_fin,
        "sentence_finite": s_fin,
        "last_hidden_finite": h_fin,
        "sentence_max_abs": finite_max(sent),
        "last_hidden_max_abs": finite_max(hid),
        "sentence_nan_inf": int((~np.isfinite(sent)).sum()),
    }


def check_cases(path, provider, device_id, table_path):
    L = int(os.path.basename(path).split("_")[-1].split(".")[0])
    sess = open_session(path, provider, device_id)
    providers = sess.get_providers()
    types = {i.name: i.type for i in sess.get_inputs()}
    if "inputs_embeds" not in types:
        raise SystemExit("zero_check checks inputs_embeds models only; this model has in-graph lookup "
                         f"inputs {sorted(types)} and is rejected")
    dt = np.float16 if "float16" in types["inputs_embeds"] else np.float32
    table = np.load(table_path, mmap_mode="r")
    ids = np.arange(2, L + 2)
    cases = {
        "a_zero_embeds_mask_ones": {"inputs_embeds": np.zeros((1, L, 512), dtype=dt),
                                    "attention_mask": np.ones((1, L), dtype=np.int64)},
        "b_table_rows_mask_zeros": {"inputs_embeds": np.asarray(table[ids], dtype=dt)[None],
                                    "attention_mask": np.zeros((1, L), dtype=np.int64)},
    }
    out = {"length": L, "session_providers": providers}
    for name, feed in cases.items():
        out[name] = run_case(sess, feed)
    out["all_finite"] = all(out[n]["finite"] for n in cases)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--lengths", default="32,128")
    ap.add_argument("--provider", choices=["cpu", "dml"], default="cpu")
    ap.add_argument("--device-id", type=int, default=0, help="DirectML device index (dml only)")
    args = ap.parse_args()
    table_path = os.path.join(args.dir, TABLE_NAME)
    res = {}
    ok = True
    for L in [int(x) for x in args.lengths.split(",")]:
        path = os.path.join(args.dir, f"model_{L}.onnx")
        try:
            r = check_cases(path, args.provider, args.device_id, table_path)
            res[str(L)] = r
            ok = ok and r["all_finite"]
        except SystemExit:
            raise
        except Exception as exc:
            res[str(L)] = {"error": f"{type(exc).__name__}: {exc}"[:800]}
            ok = False
    res["provider"] = args.provider
    res["device_id"] = args.device_id if args.provider == "dml" else None
    res["all_finite"] = ok
    print(json.dumps(res, indent=1))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
