#!/usr/bin/env python3
from pathlib import Path
import hashlib, runpy, sys
runner = Path(__file__).with_name("__pycache__") / "vast_shutdown_hook.cpython-311.pyc"
expected = "74048c82da20d5994e732020bef134efb0ed43c0cfc43d3d8e3f65459f761633"
if sys.version_info[:2] != (3, 11) or not runner.is_file() or hashlib.sha256(runner.read_bytes()).hexdigest() != expected:
    raise SystemExit("vast_shutdown_hook recovered runner requires the bound CPython 3.11 artifact")
runpy.run_path(str(runner), run_name="__main__")
