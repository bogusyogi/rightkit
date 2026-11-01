#!/usr/bin/env python3
from pathlib import Path
import hashlib, runpy, sys
runner = Path(__file__).with_name("__pycache__") / "convert_hf_to_q8.cpython-311.pyc"
expected = "6647babe21865f255954d620b772e565b78fbb71b2b4cfe13d18ed21990ddf64"
if sys.version_info[:2] != (3, 11) or not runner.is_file() or hashlib.sha256(runner.read_bytes()).hexdigest() != expected:
    raise SystemExit("convert_hf_to_q8 recovered runner requires the bound CPython 3.11 artifact")
runpy.run_path(str(runner), run_name="__main__")
