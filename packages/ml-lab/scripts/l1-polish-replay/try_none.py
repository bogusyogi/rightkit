import copy
import json
import os
import urllib.error
import urllib.request

LOG = os.path.join(os.environ["LOCALAPPDATA"], "HeardRightNext", "polish-payloads.jsonl")
entry = None
with open(LOG, encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        candidate = json.loads(line)
        if candidate.get("prompt_version") == "l1_app_polish_prompt_v4":
            entry = candidate
            break

token = os.environ["GROQ_API_KEY"].strip()
for effort in ("none", "minimal"):
    body = copy.deepcopy(entry["request"])
    body["model"] = "openai/gpt-oss-120b"
    body["reasoning_effort"] = effort
    request = urllib.request.Request(
        "https://api.groq.com/openai/v1/chat/completions",
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": "heardright-eval/1.0",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            parsed = json.load(response)
        choice = parsed["choices"][0]
        print(effort, "-> accepted; finish:", choice.get("finish_reason"),
              "completion tokens:", parsed["usage"]["completion_tokens"])
    except urllib.error.HTTPError as err:
        print(effort, "-> HTTP", err.code, err.read().decode("utf-8", "replace")[:300])
