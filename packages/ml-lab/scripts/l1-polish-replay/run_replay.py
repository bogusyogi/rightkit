"""Replay logged l1_app_polish_prompt_v4 payloads against candidate models.

Run packet: RUN_PACKET.md (same directory). Candidates see only the verbatim
runtime payload from polish-payloads.jsonl with the model id (and provider
reasoning options) swapped per lane. No authority, scoring, or clip metadata
is visible to any candidate.

Usage:
  python run_replay.py --smoke   # 2 payloads x every lane
  python run_replay.py           # full 30-payload subset, resumable
"""

import argparse
import copy
import json
import os
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(
    os.environ["LOCALAPPDATA"], "HeardRightNext", "polish-payloads.jsonl"
)
RESULTS = os.path.join(HERE, "results.jsonl")
PROMPT_VERSION = "l1_app_polish_prompt_v4"

# 1-based chronological indices within the v4 entries (see RUN_PACKET.md).
SUBSET = [3, 5, 8, 18, 19, 21, 23, 25, 26, 29, 31, 34, 35, 39, 41, 43, 44, 46,
          49, 53, 57, 58, 59, 63, 67, 75, 76, 78, 80, 82]

LANES = [
    # (lane id, provider, base_url, key env, model, reasoning overrides)
    ("groq_gpt-oss-120b_asShipped", "groq", "https://api.groq.com/openai/v1",
     "GROQ_API_KEY", "openai/gpt-oss-120b", {"drop": True}),
    ("groq_gpt-oss-120b_low", "groq", "https://api.groq.com/openai/v1",
     "GROQ_API_KEY", "openai/gpt-oss-120b", {"reasoning_effort": "low"}),
    ("groq_gpt-oss-20b_low", "groq", "https://api.groq.com/openai/v1",
     "GROQ_API_KEY", "openai/gpt-oss-20b", {"reasoning_effort": "low"}),
    ("groq_llama-3.3-70b", "groq", "https://api.groq.com/openai/v1",
     "GROQ_API_KEY", "llama-3.3-70b-versatile", {"drop": True}),
    ("groq_llama-3.1-8b", "groq", "https://api.groq.com/openai/v1",
     "GROQ_API_KEY", "llama-3.1-8b-instant", {"drop": True}),
    ("cerebras_gpt-oss-120b", "cerebras", "https://api.cerebras.ai/v1",
     "CEREBRAS_API_KEY", "gpt-oss-120b",
     {"reasoning_effort": "low", "reasoning_format": "hidden"}),
]


def load_v4_entries():
    entries = []
    with open(LOG, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            entry = json.loads(line)
            if entry.get("prompt_version") == PROMPT_VERSION:
                entries.append(entry)
    return entries


def build_body(logged_request, model, overrides):
    body = copy.deepcopy(logged_request)
    body["model"] = model
    body.pop("reasoning_effort", None)
    body.pop("reasoning_format", None)
    if not overrides.get("drop"):
        for key, value in overrides.items():
            body[key] = value
    return body


def call(base_url, key_env, body):
    token = os.environ[key_env].strip()
    data = json.dumps(body).encode("utf-8")
    request = urllib.request.Request(
        base_url + "/chat/completions",
        data=data,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": "heardright-eval/1.0",
            "Accept": "application/json",
        },
    )
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.load(response), time.time() - started, None
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", "replace")[:400]
        retry_after = err.headers.get("retry-after")
        return None, time.time() - started, (err.code, detail, retry_after)
    except Exception as err:  # noqa: BLE001 — recorded, lane keeps going
        return None, time.time() - started, (0, str(err)[:400], None)


def done_keys():
    keys = set()
    if os.path.exists(RESULTS):
        with open(RESULTS, encoding="utf-8") as fh:
            for line in fh:
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if row.get("ok"):
                    keys.add((row["lane"], row["idx"]))
    return keys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--smoke", action="store_true")
    args = parser.parse_args()

    entries = load_v4_entries()
    subset = SUBSET[:2] if args.smoke else SUBSET
    finished = done_keys()

    with open(RESULTS, "a", encoding="utf-8") as out:
        for lane, provider, base_url, key_env, model, overrides in LANES:
            consecutive_failures = 0
            for idx in subset:
                if (lane, idx) in finished:
                    continue
                if consecutive_failures >= 3:
                    print(f"[{lane}] circuit open, skipping rest", flush=True)
                    break
                logged = entries[idx - 1]
                body = build_body(logged["request"], model, overrides)
                response, elapsed, error = call(base_url, key_env, body)
                if error and error[0] in (429, 500, 502, 503):
                    wait = float(error[2]) if error[2] else 5.0
                    time.sleep(min(wait, 30.0))
                    response, elapsed, error = call(base_url, key_env, body)
                row = {
                    "lane": lane,
                    "provider": provider,
                    "model": model,
                    "idx": idx,
                    "ok": error is None,
                    "wall_s": round(elapsed, 3),
                }
                if error is None:
                    choice = response["choices"][0]
                    message = choice.get("message") or {}
                    content = message.get("content")
                    if content is None:
                        # e.g. reasoning-only replies: record the shape, score as invalid
                        row["ok"] = False
                        row["error"] = {
                            "code": -1,
                            "detail": "no_content; message keys: "
                            + ",".join(sorted(message.keys())),
                        }
                        consecutive_failures += 1
                    else:
                        row["output"] = content
                        row["finish_reason"] = choice.get("finish_reason")
                        row["usage"] = response.get("usage")
                        consecutive_failures = 0
                else:
                    row["error"] = {"code": error[0], "detail": error[1]}
                    consecutive_failures += 1
                out.write(json.dumps(row, ensure_ascii=False) + "\n")
                out.flush()
                status = "ok" if error is None else f"ERR {error[0]}"
                print(f"[{lane}] #{idx} {status} {elapsed:.2f}s", flush=True)
                time.sleep(1.1)
    print("done")


if __name__ == "__main__":
    main()
