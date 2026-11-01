import json
import os
import re
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
LOG = os.path.join(os.environ["LOCALAPPDATA"], "HeardRightNext", "polish-payloads.jsonl")
lane_arg = sys.argv[1]
indices = [int(x) for x in sys.argv[2].split(",")]

entries = []
with open(LOG, encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        entry = json.loads(line)
        if entry.get("prompt_version") == "l1_app_polish_prompt_v4":
            entries.append(entry)

rows = {}
with open("scripts/l1-polish-replay/results.jsonl", encoding="utf-8") as fh:
    for line in fh:
        row = json.loads(line)
        if row["lane"] == lane_arg and row.get("ok"):
            rows[row["idx"]] = row

for idx in indices:
    entry = entries[idx - 1]
    user_msg = entry["request"]["messages"][-1]["content"]
    transcript = re.search(r"<transcript>\n(.*?)\n</transcript>", user_msg, re.S).group(1)
    control = json.loads(entry["response"])["choices"][0]["message"]["content"]
    candidate = rows.get(idx, {}).get("output", "(missing)")
    print(f"===== #{idx}")
    print("IN     :", transcript.replace("\n", " / "))
    print("QWEN   :", control.replace("\n", " / "))
    print("CAND   :", candidate.replace("\n", " / "))
    print()
