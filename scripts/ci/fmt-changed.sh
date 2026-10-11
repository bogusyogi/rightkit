#!/usr/bin/env bash
# Format (or with --check, verify) every Rust file changed since a base commit, each with its own
# crate's edition. Uses the standalone formatter only: nothing is compiled, so it is safe to run
# on a developer host. CI's `fmt` unit stays the authority.
#   scripts/ci/fmt-changed.sh [--check] [base]     base defaults to origin/main
set -euo pipefail
cd "$(dirname "$0")/../.."
mode=()
if [[ "${1:-}" == "--check" ]]; then mode=(--check); shift; fi
base="${1:-origin/main}"
status=0
while IFS= read -r file; do
  [[ -f "$file" ]] || continue
  dir="$(dirname "$file")"
  while [[ "$dir" != "." && ! -f "$dir/Cargo.toml" ]]; do dir="$(dirname "$dir")"; done
  edition="$(sed -n 's/^edition *= *"\([0-9]*\)".*/\1/p' "$dir/Cargo.toml" 2>/dev/null | head -1)"
  rustfmt --edition "${edition:-2021}" ${mode[@]+"${mode[@]}"} "$file" || status=1
done < <({ git diff --name-only "$base" -- '*.rs'; git ls-files --others --exclude-standard -- '*.rs'; } | sort -u)
exit "$status"
