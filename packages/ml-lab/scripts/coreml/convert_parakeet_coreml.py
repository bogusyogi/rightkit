#!/usr/bin/env python3
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).with_name("impl")))
from convert_parakeet_coreml_impl import *  # noqa: F401,F403,E402
from convert_parakeet_coreml_impl import main  # noqa: E402

if __name__ == "__main__":
    raise SystemExit(main())
