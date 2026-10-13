#!/usr/bin/env bash
# Best-effort: record the normal-dependency graph of each RightKit owner crate, with default features
# and with --no-default-features, into the QA evidence directory (uploaded as qa-evidence-<runner>-<attempt>).
# Output: <evidence>/owner-dep-graphs/<crate>-{default,nodefault}.txt and summary.tsv.
# Never fails the gate. cargo tree only resolves; --locked keeps it from rewriting Cargo.lock.
set -uo pipefail
cd "$(dirname "$0")/../.."

EVIDENCE="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/rightkit-ci"
OUT="$EVIDENCE/owner-dep-graphs"
CRATES="shell tauri jobs process framed-sidecar service daemon fs db settings secrets license logs crash http qa control mcp"
WATCH="tokio axum hyper ring cc openssl-sys reqwest libsqlite3-sys"

mkdir -p "$OUT" || { echo "owner-dep-graphs: warning: cannot create $OUT"; exit 0; }
{
  printf 'crate\tmode\tunique_packages'
  for w in $WATCH; do printf '\t%s' "$w"; done
  printf '\n'
} > "$OUT/summary.tsv"

for short in $CRATES; do
  crate="rightkit-$short"
  for mode in default nodefault; do
    flags=""
    [ "$mode" = "nodefault" ] && flags="--no-default-features"
    file="$OUT/$crate-$mode.txt"
    # shellcheck disable=SC2086
    if cargo tree --locked -e normal -p "$crate" --prefix none $flags > "$file" 2> "$file.err"; then
      rm -f "$file.err"
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
  done
done
echo "owner-dep-graphs: wrote $OUT"
exit 0
