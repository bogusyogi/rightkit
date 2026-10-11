#!/usr/bin/env bash
# RightKit CI gate. Thin caller: all selection and execution lives in scripts/ci/gate.mjs.
set -euo pipefail
cd "$(dirname "$0")/.."
exec node scripts/ci/gate.mjs "$@"
