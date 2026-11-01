import json
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
rows = []
for line in open("scripts/l1-polish-replay/results.jsonl", encoding="utf-8"):
    row = json.loads(line)
    if row["lane"] != "groq_gpt-oss-120b_low" or not row.get("ok"):
        continue
    usage = row.get("usage") or {}
    completion = usage.get("completion_tokens")
    if completion is None:
        continue
    visible = max(1, len(row.get("output") or "") // 4)
    rows.append((row["idx"], row.get("finish_reason"), completion, visible,
                 completion - visible))

reasoning = sorted(r[4] for r in rows if r[1] == "stop")
print("stop rows:", len(reasoning), "reasoning-token overhead min/median/p90/max:",
      reasoning[0], reasoning[len(reasoning) // 2],
      reasoning[int(len(reasoning) * 0.9)], reasoning[-1])
for r in rows:
    if r[1] == "length":
        print("truncated idx", r[0], "completion", r[2], "visible-est", r[3])
