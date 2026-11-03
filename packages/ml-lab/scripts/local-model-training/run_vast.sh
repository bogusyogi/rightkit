#!/usr/bin/env bash
set -euo pipefail

NODE_VERSION="24.18.0"
NODE_ARCHIVE="node-v${NODE_VERSION}-linux-x64.tar.xz"
NODE_SHA256="55aa7153f9d88f28d765fcdad5ae6945b5c0f98a36881703817e4c450fa76742"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE_DIR="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
RUNTIME_DIR="${BUNDLE_DIR}/.runtime/node-v${NODE_VERSION}-linux-x64"
NODE_BIN="${RUNTIME_DIR}/bin/node"
RIGHTKIT_ML_PYTHON="${RIGHTKIT_ML_PYTHON:-/workspace/heardright-runtime-cpython311/bin/python3}"
RIGHTKIT_ML_RUNTIME_DIR="${RIGHTKIT_ML_RUNTIME_DIR:-/workspace/heardright-v15-shared-runtime}"

if [[ ! -x "${NODE_BIN}" ]]; then
  TEMP_DIR="$(mktemp -d)"
  trap 'rm -rf -- "${TEMP_DIR}"' EXIT
  curl --fail --location --proto '=https' --tlsv1.2 \
    "https://nodejs.org/download/release/v${NODE_VERSION}/${NODE_ARCHIVE}" \
    --output "${TEMP_DIR}/${NODE_ARCHIVE}"
  printf '%s  %s\n' "${NODE_SHA256}" "${TEMP_DIR}/${NODE_ARCHIVE}" | sha256sum --check --status
  mkdir -p -- "${BUNDLE_DIR}/.runtime"
  tar -xJf "${TEMP_DIR}/${NODE_ARCHIVE}" -C "${BUNDLE_DIR}/.runtime"
fi

ACTUAL_VERSION="$(${NODE_BIN} --version)"
[[ "${ACTUAL_VERSION}" == "v${NODE_VERSION}" ]] || { printf 'Unexpected Node version: %s\n' "${ACTUAL_VERSION}" >&2; exit 1; }
export PATH="${RUNTIME_DIR}/bin:${PATH}"
[[ -x "${RIGHTKIT_ML_PYTHON}" ]] || { printf 'Recovered CPython 3.11 is missing: %s\n' "${RIGHTKIT_ML_PYTHON}" >&2; exit 1; }
export RIGHTKIT_ML_PYTHON
export RIGHTKIT_ML_RUNTIME_DIR
exec "${NODE_BIN}" "${SCRIPT_DIR}/launch_vast.mjs" "$@"
