#!/bin/sh
# Run a Rust UI test target (rightkit-qa scenarios that launch a real .app) from the user's
# login session. `rustright cargo test` runs test binaries in the build broker's context, where
# macOS `open` has no GUI session (exit 1) and the environment is scrubbed. This builds the
# test through the broker, then executes the binary here, with this shell's environment.
#
#   scripts/run-ui-tests.sh -p cutright-e2e --test ui [--features full] [-- <libtest args>]
#
# Superseded by `rightkit-qa run-tests` (crates/rightkit-qa/docs/run-tests.md): the same compile-in-the-broker, run-outside
# step with deadlines, parallelism, a receipt and the process tree killed at a deadline. Delete this script once its callers move.
set -eu
build_args=""
while [ $# -gt 0 ] && [ "$1" != "--" ]; do build_args="$build_args $1"; shift; done
[ "${1:-}" = "--" ] && shift
# shellcheck disable=SC2086
out="$(rustright cargo test $build_args --no-run 2>&1)" || { printf '%s\n' "$out" >&2; exit 1; }
bins="$(printf '%s\n' "$out" | sed -n 's/.*Executable [^(]*(\(.*\))$/\1/p')"
[ -n "$bins" ] || { printf '%s\n' "$out" >&2; echo "run-ui-tests: no test executable built" >&2; exit 1; }
status=0
for b in $bins; do
  echo "run-ui-tests: $b"
  "$b" "$@" || status=$?
done
exit "$status"
