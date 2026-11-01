#!/usr/bin/env python3
from pathlib import Path
import hashlib, runpy, sys
runner = Path(__file__).with_name("__pycache__") / "capture_native_assets.cpython-311.pyc"
expected = "cdd61fc1107257035daf3882232187c310bf8da2e235951a59a34cb36049f88f"
if sys.version_info[:2] != (3, 11) or not runner.is_file() or hashlib.sha256(runner.read_bytes()).hexdigest() != expected:
    raise SystemExit("capture_native_assets recovered runner requires the bound CPython 3.11 artifact")
runpy.run_path(str(runner), run_name="__main__")
