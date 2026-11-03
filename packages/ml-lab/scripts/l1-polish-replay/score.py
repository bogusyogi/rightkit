"""Score replay results per RUN_PACKET.md rubric. Scorer-side only; runs after
all provider calls are captured and makes no provider calls."""

import json
import os
import re
import statistics
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(os.environ["LOCALAPPDATA"], "HeardRightNext", "polish-payloads.jsonl")
RESULTS = os.path.join(HERE, "results.jsonl")
PROMPT_VERSION = "l1_app_polish_prompt_v4"
SUBSET = [3, 5, 8, 18, 19, 21, 23, 25, 26, 29, 31, 34, 35, 39, 41, 43, 44, 46,
          49, 53, 57, 58, 59, 63, 67, 75, 76, 78, 80, 82]

def load_inputs():
    entries = []
    with open(LOG, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            entry = json.loads(line)
            if entry.get("prompt_version") == PROMPT_VERSION:
                entries.append(entry)
    inputs = {}
    for pos, entry in enumerate(entries, start=1):
        user_msg = entry["request"]["messages"][-1]["content"]
        match = re.search(r"<transcript>\n(.*?)\n</transcript>", user_msg, re.S)
        control_output = ""
        try:
            parsed = json.loads(entry["response"])
            control_output = parsed["choices"][0]["message"]["content"]
        except Exception:
            pass
        inputs[pos] = {
            "transcript": match.group(1) if match else "",
            "control": control_output,
        }
    return inputs

FORMAT_RX = re.compile(
    r"^```|^(cleaned transcript|polished output|here is|here's|sure[,!]|transcript)\b",
    re.I,
)

def flags(transcript, output):
    out = {}
    out["dash"] = ("—" in output) or ("–" in output)
    out["format"] = bool(FORMAT_RX.search(output.strip()))
    ratio = len(output) / max(1, len(transcript))
    out["len_flag"] = not (0.55 <= ratio <= 1.45)
    out["ratio"] = round(ratio, 2)
    q_in = transcript.count("?")
    q_out = output.count("?")
    out["q_drop"] = q_out < q_in
    return out

def main():
    inputs = load_inputs()
    lanes = {}
    with open(RESULTS, encoding="utf-8") as fh:
        for line in fh:
            row = json.loads(line)
            lanes.setdefault(row["lane"], {})[row["idx"]] = row
    lanes["CONTROL_qwen3.6-27b"] = {
        idx: {"ok": True, "output": inputs[idx]["control"], "finish_reason": "stop",
              "usage": {"total_time": None}}
        for idx in SUBSET
    }

    for lane, rows in lanes.items():
        calls = [rows[i] for i in SUBSET if i in rows]
        errors = [r for r in calls if not r.get("ok")]
        oks = [r for r in calls if r.get("ok")]
        truncated = [r for r in oks if r.get("finish_reason") == "length"]
        dash = []
        fmt = []
        len_flags = []
        q_drops = []
        for r in oks:
            idx = SUBSET.index([k for k, v in rows.items() if v is r][0]) if False else None
        latencies = []
        for i in SUBSET:
            r = rows.get(i)
            if not r or not r.get("ok"):
                continue
            f = flags(inputs[i]["transcript"], r.get("output") or "")
            if f["dash"]:
                dash.append(i)
            if f["format"]:
                fmt.append(i)
            if f["len_flag"]:
                len_flags.append((i, f["ratio"]))
            if f["q_drop"]:
                q_drops.append(i)
            usage = r.get("usage") or {}
            if usage.get("total_time"):
                latencies.append(usage["total_time"])
        line = {
            "lane": lane,
            "calls": len(calls),
            "errors": len(errors),
            "truncated": [i for i in SUBSET if rows.get(i, {}).get("finish_reason") == "length"],
            "dash_violations": dash,
            "format_violations": fmt,
            "len_flags": len_flags,
            "q_drops": q_drops,
        }
        if latencies:
            line["latency_p50"] = round(statistics.median(latencies), 2)
            line["latency_max"] = round(max(latencies), 2)
        print(json.dumps(line, ensure_ascii=False))

if __name__ == "__main__":
    main()
