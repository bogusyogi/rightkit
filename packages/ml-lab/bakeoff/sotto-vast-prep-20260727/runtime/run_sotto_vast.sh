#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${RIGHTKIT_ML_PYTHON:-/workspace/heardright-runtime-cpython311/bin/python3}"
AUTH="${1:-$ROOT/authorization/full.json}"
[[ -x "${PYTHON}" ]] || { echo "missing CPython 3.11: ${PYTHON}" >&2; exit 64; }
[[ -f "${AUTH}" ]] || { echo "missing authorization: ${AUTH}" >&2; exit 64; }
python3 - "${ROOT}/environment.lock.json" "${AUTH}" <<'PY'
import json, sys
lock = json.load(open(sys.argv[1], encoding="utf-8"))
auth = json.load(open(sys.argv[2], encoding="utf-8"))
if lock.get("status") != "resolved_pass_live":
    raise SystemExit("environment lock is not resolved_pass_live")
if auth.get("approved") is not True:
    raise SystemExit("authorization approved flag is false")
if auth.get("hourly_rate_usd", 0) <= 0 or auth.get("max_usd", 0) <= 0:
    raise SystemExit("authorization must bind positive hourly rate and max USD")
PY
exec "${PYTHON}" "${ROOT}/train.py" --family sotto --action train --output-dir "${OUTPUT_DIR:-/workspace/heardright-sotto-full}" --model-source juanquivilla/sotto-cleanup-lfm25-350m --model-revision 6df6f019170b8b55333c047b901886a51750a965 --live --training-input "${ROOT}/data/train.jsonl" --mode full --epochs 3 --batch-size 2 --gradient-accumulation 8
