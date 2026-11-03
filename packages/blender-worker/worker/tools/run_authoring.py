#!/usr/bin/env python3
"""Run a local authoring job with an external deadline, including blocking renders."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("job", type=Path)
    parser.add_argument("--blender", required=True, type=Path)
    parser.add_argument("--budget-seconds", type=int, default=1800)
    args = parser.parse_args()
    if not 1 <= args.budget_seconds <= 1800:
        parser.error("authoring budget must be 1..1800 seconds")
    job_path = args.job.resolve(strict=True)
    job_bytes = job_path.read_bytes()
    job = json.loads(job_bytes)
    if job.get("kind") != "authoring" or job.get("recipe") not in ("lookdev", "measure", "swatch"):
        parser.error("this launcher accepts only authoring jobs")
    receipt_path = job_path.with_suffix(".launch.json")
    if receipt_path.exists():
        parser.error("launch receipt exists; use a new job identity for a new experiment")
    started = time.time()
    receipt = {"job_sha256": hashlib.sha256(job_bytes).hexdigest(), "job_id": job.get("job_id"),
               "start_unix": started, "deadline_unix": started + args.budget_seconds,
               "started_at": datetime.now(timezone.utc).isoformat(), "state": "starting"}
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    entry = Path(__file__).resolve().parents[1] / "render_job.py"
    log = job_path.with_suffix(".blender.log")
    command = [str(args.blender.resolve(strict=True)), "-b", "--factory-startup", "--python-exit-code", "1",
               "--python", str(entry), "--", str(job_path)]
    with log.open("wb") as stream:
        child = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT,
                                 creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        receipt.update(state="running", pid=child.pid, command=command)
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"event": "AUTHORING_STARTED", "job_id": job.get("job_id"), "pid": child.pid,
                          "log": str(log), "deadline_unix": receipt["deadline_unix"]}), flush=True)
        try:
            code = child.wait(timeout=max(.001, receipt["deadline_unix"] - time.time()))
            receipt.update(state="complete" if code == 0 else "failed", exit_code=code)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            code = 124
            receipt.update(state="deadline_exceeded", exit_code=code)
    receipt["elapsed_seconds"] = time.time() - started
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event": "AUTHORING_EXIT", **receipt}), flush=True)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
