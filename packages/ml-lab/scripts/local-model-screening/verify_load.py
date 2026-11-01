#!/usr/bin/env python3
"""Load-smoke downloaded GGUFs and require real GPU layer offload."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_MANIFEST = SCRIPT_DIR / "manifest.json"


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--root", type=Path, default=Path("E:/models/heardright-local-screening"))
    parser.add_argument("--llama-cli", type=Path, required=True)
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument("--timeout", type=int, default=180)
    return parser.parse_args()


def parse_offload(output: str) -> tuple[int | None, int | None]:
    matches = re.findall(r"offloaded\s+(\d+)\s*/\s*(\d+)\s+layers\s+to\s+GPU", output)
    if not matches:
        return None, None
    loaded, total = matches[-1]
    return int(loaded), int(total)


def smoke_one(args: argparse.Namespace, model: dict[str, Any]) -> dict[str, Any]:
    model_path = args.root / "models" / model["id"] / model["filename"]
    if not model_path.exists():
        raise RuntimeError(f"missing model: {model_path}")
    command = [
        str(args.llama_cli),
        "--model",
        str(model_path),
        "--ctx-size",
        "512",
        "--n-predict",
        "1",
        "--gpu-layers",
        "999",
        "--threads",
        "4",
        "--seed",
        "42",
        "--temp",
        "0",
        "--no-display-prompt",
        "--single-turn",
        "--simple-io",
        "--verbosity",
        "4",
        "--log-colors",
        "off",
        "--prompt",
        "Hello",
    ]
    env = {**os.environ, "NO_COLOR": "1", "TERM": "dumb"}
    # A llama.cpp console process attached to Codex's Windows console can
    # propagate CTRL_CLOSE_EVENT back into the app-server process group. Run it
    # without an inherited console; macOS/Linux do not need creation flags.
    creationflags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    started = time.perf_counter()
    completed = subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=args.timeout,
        env=env,
        creationflags=creationflags,
        check=False,
    )
    elapsed_ms = round((time.perf_counter() - started) * 1000, 1)
    combined = completed.stdout + "\n" + completed.stderr
    gpu_layers, total_layers = parse_offload(combined)
    backend = "CUDA" if "CUDA" in combined else ("Vulkan" if "Vulkan" in combined else "unknown")
    success = completed.returncode == 0 and gpu_layers is not None and gpu_layers > 0
    return {
        "id": model["id"],
        "model": str(model_path),
        "backend": backend,
        "returncode": completed.returncode,
        "elapsed_ms": elapsed_ms,
        "gpu_layers": gpu_layers,
        "total_layers": total_layers,
        "success": success,
        "output_tail": combined[-4000:],
    }


def main() -> int:
    args = parse_args()
    if not args.llama_cli.exists():
        raise SystemExit(f"llama-cli does not exist: {args.llama_cli}")
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    selected = [
        model for model in manifest["models"] if not args.only or model["id"] in set(args.only)
    ]
    unknown = sorted(set(args.only) - {model["id"] for model in manifest["models"]})
    if unknown:
        raise SystemExit(f"Unknown candidate id(s): {', '.join(unknown)}")

    results: list[dict[str, Any]] = []
    for index, model in enumerate(selected, start=1):
        print(f"[LOAD {index}/{len(selected)}] {model['id']}", flush=True)
        try:
            result = smoke_one(args, model)
        except Exception as exc:  # noqa: BLE001 - preserve every failure in report
            result = {"id": model["id"], "success": False, "error": str(exc)}
        results.append(result)
        if result["success"]:
            print(
                f"[OK] {model['id']} backend={result['backend']} "
                f"offload={result['gpu_layers']}/{result['total_layers']} elapsed={result['elapsed_ms']}ms",
                flush=True,
            )
        else:
            print(f"[FAIL] {model['id']}: {result.get('error') or result.get('output_tail', '')[-500:]}", flush=True)

    report = {
        "schema": "heardright.local_model_screening_load_smoke.v1",
        "completed_at": now_iso(),
        "llama_cli": str(args.llama_cli.resolve()),
        "requested": len(selected),
        "passed": sum(1 for result in results if result["success"]),
        "results": results,
    }
    report_path = args.root / "load-smoke-results.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Passed: {report['passed']}/{report['requested']} | report: {report_path}")
    return 0 if report["passed"] == report["requested"] else 1


if __name__ == "__main__":
    sys.exit(main())
