#!/usr/bin/env bash
# RightKit CI gate. Thin caller: all selection and execution lives in scripts/ci/gate.mjs.
set -euo pipefail
cd "$(dirname "$0")/.."
# Best-effort evidence (owner crate dependency graphs); never affects the gate result.
bash scripts/ci/owner-dep-graphs.sh || echo "owner-dep-graphs: warning: skipped"
exec node scripts/ci/gate.mjs "$@"
