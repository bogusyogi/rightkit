#!/usr/bin/env python3
"""Fail-closed billing guard for a supervised Vast training run."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
from typing import Any


TERMINAL_STATUSES = {
    "complete",
    "complete_with_rejections",
    "completed_no_viable_candidates",
    "failed",
}
PROGRESS_DIRS = ("stage-receipts", "sync-requests", "sync-receipts")


def read_json(path: Path) -> dict[str, Any] | None:
    try:
        return json.loads(path.read_text("utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def process_matches(token: str) -> list[int]:
    matches: list[int] = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            command = (
                (entry / "cmdline")
                .read_bytes()
                .replace(b"\0", b" ")
                .decode("utf-8", "replace")
            )
        except OSError:
            continue
        if (
            token in command
            and "paid_run_watchdog" not in command
            and "paid-run-watchdog" not in command
        ):
            matches.append(int(entry.name))
    return matches


def latest_progress_mtime(run_root: Path) -> float:
    latest = 0.0
    candidates = [run_root / "state.json"]
    for directory_name in PROGRESS_DIRS:
        directory = run_root / directory_name
        if directory.is_dir():
            candidates.extend(path for path in directory.rglob("*") if path.is_file())
    for path in candidates:
        try:
            latest = max(latest, path.stat().st_mtime)
        except OSError:
            continue
    return latest


def gpu_utilization() -> float | None:
    result = subprocess.run(
        [
            "nvidia-smi",
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    try:
        values = [
            float(line.strip())
            for line in result.stdout.splitlines()
            if line.strip()
        ]
        return max(values) if values else None
    except ValueError:
        return None


def workload_cpu_utilization(pids: list[int]) -> float | None:
    if not pids:
        return 0.0
    result = subprocess.run(
        ["ps", "-p", ",".join(str(pid) for pid in pids), "-o", "%cpu="],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    try:
        return sum(
            float(line.strip())
            for line in result.stdout.splitlines()
            if line.strip()
        )
    except ValueError:
        return None


def free_bytes(path: Path) -> int:
    return shutil.disk_usage(path).free


def decision(
    *,
    now: float,
    installed_at: float,
    state: dict[str, Any] | None,
    matching_pids: list[int],
    progress_mtime: float,
    gpu_util: float | None,
    workload_cpu_util: float | None,
    free_disk_bytes: int,
    startup_grace_seconds: int,
    progress_timeout_seconds: int,
    terminal_grace_seconds: int,
    min_free_bytes: int,
) -> dict[str, Any]:
    status = state.get("status") if state else None
    events = state.get("events") if state else None
    terminal_at = None
    if status in TERMINAL_STATUSES and isinstance(events, list):
        terminal_events = [
            event.get("at")
            for event in events
            if isinstance(event, dict)
            and event.get("kind") in {"run_complete", "run_failed"}
        ]
        if terminal_events:
            try:
                terminal_at = time.mktime(
                    time.strptime(terminal_events[-1], "%Y-%m-%dT%H:%M:%S.%fZ")
                )
            except (TypeError, ValueError):
                terminal_at = progress_mtime or now
    if status in TERMINAL_STATUSES:
        if (state or {}).get("final_sync_verified") is True:
            return {
                "action": "stop",
                "reason": "terminal_sync_verified",
                "status": status,
            }
        terminal_age = max(0.0, now - (terminal_at or progress_mtime or now))
        if terminal_age >= terminal_grace_seconds:
            return {
                "action": "stop",
                "reason": "terminal_sync_grace_exhausted",
                "status": status,
            }
        return {
            "action": "wait",
            "reason": "terminal_sync_grace",
            "status": status,
        }
    if free_disk_bytes < min_free_bytes:
        return {
            "action": "stop",
            "reason": "disk_reserve_breached",
            "free_disk_bytes": free_disk_bytes,
        }
    # Startup grace is unconditional. A matching launcher PID must not expose a
    # fresh run to stale progress files left by an earlier interrupted attempt.
    if now - installed_at < startup_grace_seconds:
        return {
            "action": "wait",
            "reason": "startup_grace",
            "status": status,
        }
    if not matching_pids:
        return {
            "action": "stop",
            "reason": "authorized_run_process_missing",
            "status": status,
        }
    progress_age = max(0.0, now - (progress_mtime or installed_at))
    low_gpu = gpu_util is None or gpu_util < 5
    low_cpu = workload_cpu_util is None or workload_cpu_util < 25
    if progress_age >= progress_timeout_seconds and low_gpu and low_cpu:
        return {
            "action": "stop",
            "reason": "run_stalled_without_gpu_or_file_progress",
            "progress_age_seconds": progress_age,
            "gpu_utilization": gpu_util,
            "workload_cpu_utilization": workload_cpu_util,
        }
    return {
        "action": "continue",
        "reason": "run_advancing",
        "status": status,
        "pids": matching_pids,
        "progress_age_seconds": progress_age,
        "gpu_utilization": gpu_util,
        "workload_cpu_utilization": workload_cpu_util,
    }


def append_event(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, sort_keys=True) + "\n")


def stop_instance(
    instance_id: str,
    event_log: Path,
    reason: str,
    dry_run: bool,
) -> None:
    event = {
        "schema_version": "heardright.l1.paid_run_watchdog_event.v1",
        "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "action": "stop_instance",
        "reason": reason,
        "instance_id": instance_id,
        "dry_run": dry_run,
    }
    if dry_run:
        append_event(event_log, event)
        return
    api_key = os.environ.get("CONTAINER_API_KEY")
    if not api_key:
        event["action"] = "stop_instance_failed"
        event["error"] = "CONTAINER_API_KEY is unavailable; cannot stop billing safely"
        append_event(event_log, event)
        raise RuntimeError(event["error"])
    result = subprocess.run(
        [
            "vastai",
            "stop",
            "instance",
            instance_id,
            "--api-key",
            api_key,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        event["action"] = "stop_instance_failed"
        event["returncode"] = result.returncode
        event["error"] = (
            f"vastai stop failed with status {result.returncode}: "
            f"{result.stderr[-2000:]}"
        )[-2500:]
        append_event(event_log, event)
        raise RuntimeError(event["error"])
    append_event(event_log, event)


def supervisor_paths(name: str) -> tuple[Path, Path]:
    if not re.fullmatch(r"[A-Za-z0-9-]+", name):
        raise ValueError("supervisor name may contain only letters, digits, and hyphens")
    return (
        Path("/opt/supervisor-scripts") / f"{name}.sh",
        Path("/etc/supervisor/conf.d") / f"{name}.conf",
    )


def install_supervisor(args: argparse.Namespace) -> None:
    script_path = Path(__file__).resolve()
    wrapper_path, config_path = supervisor_paths(args.supervisor_name)
    event_log = args.run_root / "_billing-guard" / "events.jsonl"
    command = [
        sys.executable,
        str(script_path),
        "--watch",
        "--run-root",
        str(args.run_root),
        "--command-token",
        args.command_token,
        "--instance-id",
        args.instance_id,
        "--event-log",
        str(event_log),
        "--poll-seconds",
        str(args.poll_seconds),
        "--startup-grace-seconds",
        str(args.startup_grace_seconds),
        "--progress-timeout-seconds",
        str(args.progress_timeout_seconds),
        "--terminal-grace-seconds",
        str(args.terminal_grace_seconds),
        "--min-free-gib",
        str(args.min_free_gib),
    ]
    wrapper = (
        "#!/bin/bash\nset -euo pipefail\nexec "
        + " ".join(shlex.quote(part) for part in command)
        + "\n"
    )
    config = "\n".join([
        f"[program:{args.supervisor_name}]",
        f"command={wrapper_path}",
        "autostart=true",
        "autorestart=unexpected",
        "startsecs=2",
        "startretries=3",
        "stdout_logfile=/dev/stdout",
        "redirect_stderr=true",
        "stdout_logfile_maxbytes=0",
        "",
    ])
    wrapper_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.parent.mkdir(parents=True, exist_ok=True)
    wrapper_path.write_text(wrapper, "utf-8")
    wrapper_path.chmod(0o755)
    config_path.write_text(config, "utf-8")
    for supervisor_args in (
        ["reread"],
        ["update"],
        ["restart", args.supervisor_name],
    ):
        result = subprocess.run(
            ["supervisorctl", *supervisor_args],
            check=False,
            capture_output=True,
            text=True,
        )
        if (
            result.returncode != 0
            and not (
                supervisor_args[0] == "restart"
                and "no such process" not in result.stderr.lower()
            )
        ):
            raise RuntimeError(
                f"supervisorctl {' '.join(supervisor_args)} failed: "
                f"{result.stderr[-500:]}"
            )
    receipt = {
        "schema_version": "heardright.l1.paid_run_watchdog_install.v1",
        "installed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "run_root": str(args.run_root),
        "command_token": args.command_token,
        "instance_id": args.instance_id,
        "wrapper": str(wrapper_path),
        "config": str(config_path),
        "event_log": str(event_log),
    }
    receipt_path = args.run_root / "_billing-guard" / "install.json"
    receipt_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = receipt_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n", "utf-8")
    temporary.replace(receipt_path)
    print(json.dumps({"status": "installed", "receipt": str(receipt_path)}))


def watch(args: argparse.Namespace) -> None:
    args.run_root.mkdir(parents=True, exist_ok=True)
    args.event_log.parent.mkdir(parents=True, exist_ok=True)
    installed_at = time.time()
    min_free_bytes = int(args.min_free_gib * 1024**3)
    while True:
        now = time.time()
        state = read_json(args.run_root / "state.json")
        final_receipt = read_json(
            args.run_root / "sync-receipts" / "final-run.receipt.json"
        )
        if final_receipt and final_receipt.get("verified_on_windows") is True:
            state = dict(state or {})
            state["final_sync_verified"] = True
        matching_pids = process_matches(args.command_token)
        outcome = decision(
            now=now,
            installed_at=installed_at,
            state=state,
            matching_pids=matching_pids,
            progress_mtime=latest_progress_mtime(args.run_root),
            gpu_util=gpu_utilization(),
            workload_cpu_util=workload_cpu_utilization(matching_pids),
            free_disk_bytes=free_bytes(args.run_root),
            startup_grace_seconds=args.startup_grace_seconds,
            progress_timeout_seconds=args.progress_timeout_seconds,
            terminal_grace_seconds=args.terminal_grace_seconds,
            min_free_bytes=min_free_bytes,
        )
        sample = {
            "schema_version": "heardright.l1.paid_run_watchdog_sample.v1",
            "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            **outcome,
        }
        append_event(args.event_log, sample)
        if outcome["action"] == "stop":
            stop_instance(
                args.instance_id,
                args.event_log,
                outcome["reason"],
                args.dry_run,
            )
            return
        if args.once:
            return
        time.sleep(args.poll_seconds)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--watch", action="store_true")
    mode.add_argument("--install-supervisor", action="store_true")
    parser.add_argument("--run-root", type=Path, required=True)
    parser.add_argument("--command-token", required=True)
    parser.add_argument("--instance-id", default=os.environ.get("CONTAINER_ID"))
    parser.add_argument("--event-log", type=Path)
    parser.add_argument(
        "--supervisor-name",
        default="heardright-paid-run-watchdog",
    )
    parser.add_argument("--poll-seconds", type=int, default=30)
    parser.add_argument("--startup-grace-seconds", type=int, default=180)
    parser.add_argument("--progress-timeout-seconds", type=int, default=1800)
    parser.add_argument("--terminal-grace-seconds", type=int, default=600)
    parser.add_argument("--min-free-gib", type=float, default=5.0)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    args.run_root = args.run_root.resolve()
    if not args.instance_id:
        parser.error("--instance-id or CONTAINER_ID is required")
    if args.event_log is None:
        args.event_log = args.run_root / "_billing-guard" / "events.jsonl"
    if (
        args.poll_seconds < 2
        or args.startup_grace_seconds < 10
        or args.progress_timeout_seconds < 60
    ):
        parser.error("poll/startup/progress thresholds are below the safe minimum")
    return args


def main(argv: list[str] | None = None) -> None:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.install_supervisor:
        install_supervisor(args)
    else:
        watch(args)


if __name__ == "__main__":
    main()
