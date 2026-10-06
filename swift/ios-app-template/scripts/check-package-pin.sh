#!/bin/sh
# Fails if project.yml consumes RightKitSwift by local path, or without a URL + exactVersion pin.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
spec="$here/project.yml"
fail() { echo "check-package-pin: $1" >&2; exit 1; }
awk '/^packages:/{p=1;next} /^[^ #]/{p=0} p' "$spec" | grep -Eq '^[[:space:]]+path:' && fail "local path package in project.yml"
grep -Eq '^[[:space:]]+url: https://.*rightkit-swift\.git$' "$spec" || fail "missing RightKitSwift git url"
grep -Eq '^[[:space:]]+exactVersion: [0-9]+\.[0-9]+\.[0-9]+$' "$spec" || fail "missing exactVersion pin"
grep -Eq '^[[:space:]]+(from|branch|revision|minorVersion|majorVersion|minVersion):' "$spec" && fail "non-exact requirement"
grep -Eq 'bundle\.unit-test' "$spec" && fail "unit-test target present (e2e only)"
echo "check-package-pin: OK"
