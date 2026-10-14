#!/usr/bin/env bash
# Best-effort: record the normal-dependency graph of each RightKit owner crate, with default features,
# with --no-default-features, and with the required consumer feature sets (FEATURE_SETS below; names checked
# against each crate's [features]), into the QA evidence directory (uploaded as qa-evidence-<runner>-<attempt>).
# Output: <evidence>/owner-dep-graphs/<crate>-<mode>.txt and summary.tsv (plus an ASSERT row for optional storage).
# Never fails the gate. cargo tree only resolves; --locked keeps it from rewriting Cargo.lock.
set -uo pipefail
cd "$(dirname "$0")/../.."

EVIDENCE="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/rightkit-ci"
OUT="$EVIDENCE/owner-dep-graphs"
CRATES="shell tauri job-queue child-process sidecar daemon fs db app-settings credentials license logs crash http qa control mcp"
WATCH="tokio axum hyper ring cc openssl-sys reqwest libsqlite3-sys"

# crate|mode-name|cargo feature args. default/nodefault run for every crate in CRATES; these add consumer sets.
FEATURE_SETS="
shell|nodefault-wv2-0_39|--no-default-features --features wv2-0_39
shell|nodefault-wv2-0_39-engine|--no-default-features --features wv2-0_39,engine
job-queue|nodefault-store|--no-default-features --features store
job-queue|nodefault-protocol|--no-default-features --features protocol
job-queue|nodefault-supervisor|--no-default-features --features supervisor
job-queue|nodefault-contract|--no-default-features --features contract
job-queue|nodefault-service|--no-default-features --features service
job-queue|nodefault-supervisor-crash|--no-default-features --features supervisor,crash
fs|nodefault-json|--no-default-features --features json
fs|nodefault-digest|--no-default-features --features digest
db|nodefault-bundled|--no-default-features --features bundled
credentials|nodefault-settings-probe|--no-default-features --features settings-probe
logs|nodefault-http|--no-default-features --features http
logs|nodefault-log-bridge|--no-default-features --features log-bridge
crash|nodefault-support-bundle|--no-default-features --features support-bundle
daemon|nodefault-link|--no-default-features --features link
daemon|nodefault-remote|--no-default-features --features remote
daemon|nodefault-remote-testing|--no-default-features --features remote,testing
http|nodefault-gzip|--no-default-features --features gzip
http|nodefault-async|--no-default-features --features async
http|nodefault-gzip-untrusted|--no-default-features --features gzip,untrusted
qa|nodefault-browser|--no-default-features --features browser
control|nodefault-qa-control|--no-default-features --features qa-control
mcp|nodefault-advanced-http|--no-default-features --features advanced-http
"

# Guard against a hung resolve: use GNU timeout only when it really is GNU (git-bash has it; macOS usually does not;
# Windows timeout.exe has different syntax and is ignored).
TO=""
if command -v timeout >/dev/null 2>&1 && timeout --version 2>/dev/null | grep -qi 'gnu\|coreutils'; then TO="timeout 300"; fi

mkdir -p "$OUT" || { echo "owner-dep-graphs: warning: cannot create $OUT"; exit 0; }
{
  printf 'crate\tmode\tunique_packages'
  for w in $WATCH; do printf '\t%s' "$w"; done
  printf '\n'
} > "$OUT/summary.tsv"

run_tree() { # crate mode [cargo flags...]
  local crate="$1" mode="$2"
  shift 2
  local file="$OUT/$crate-$mode.txt"
  # shellcheck disable=SC2086
  if $TO cargo tree --locked -e normal -p "$crate" --prefix none "$@" > "$file" 2> "$file.err"; then
    rm -f "$file.err"
    local count
    count=$(awk 'NF { print $1 " " $2 }' "$file" | sort -u | wc -l | tr -d ' ')
    {
      printf '%s\t%s\t%s' "$crate" "$mode" "$count"
      for w in $WATCH; do
        if awk -v w="$w" '$1 == w { f = 1 } END { exit !f }' "$file"; then printf '\tyes'; else printf '\tno'; fi
      done
      printf '\n'
    } >> "$OUT/summary.tsv"
  else
    echo "owner-dep-graphs: warning: cargo tree failed for $crate ($mode); see $file.err"
    printf '%s\t%s\terror\n' "$crate" "$mode" >> "$OUT/summary.tsv"
  fi
}

for short in $CRATES; do
  crate="rightkit-$short"
  run_tree "$crate" default
  run_tree "$crate" nodefault --no-default-features
done

while IFS='|' read -r short mode args; do
  [ -n "$short" ] || continue
  # shellcheck disable=SC2086
  run_tree "rightkit-$short" "$mode" $args
done <<TABLE
$FEATURE_SETS
TABLE

# Optional-storage validation: jobs with only `supervisor` must not pull rightkit-db / libsqlite3-sys (expected absent).
sg="$OUT/rightkit-job-queue-nodefault-supervisor.txt"
if [ -s "$sg" ]; then
  if awk '$1 == "rightkit-db" || $1 == "libsqlite3-sys" { f = 1 } END { exit !f }' "$sg"; then v=FAIL; else v=PASS; fi
else
  v=SKIP
fi
printf 'ASSERT\tjobs-supervisor-excludes-rightkit-db-and-libsqlite3-sys\t%s\n' "$v" >> "$OUT/summary.tsv"
echo "owner-dep-graphs: optional-storage assertion: $v"

# Lean-service assertion (M7 lane E): the default and link builds of rightkit-daemon carry no async server stack.
# The remote, remote,testing rows may contain tokio/axum/hyper and are not asserted.
svd="$OUT/rightkit-daemon-default.txt"
svl="$OUT/rightkit-daemon-nodefault-link.txt"
if [ -s "$svd" ] && [ -s "$svl" ]; then
  if awk '$1 == "tokio" || $1 == "axum" || $1 == "hyper" { f = 1 } END { exit !f }' "$svd" "$svl"; then v=FAIL; else v=PASS; fi
else
  v=SKIP
fi
printf 'ASSERT\tservice-default-and-link-exclude-tokio-axum-hyper\t%s\n' "$v" >> "$OUT/summary.tsv"
echo "owner-dep-graphs: service lean assertion: $v"
echo "owner-dep-graphs: wrote $OUT"
exit 0
