#!/bin/sh
# Build, Developer-ID-sign and install `right-ctl` (rightkit-agent-tools) into the managed agent-bin,
# which every agent shell has on PATH. Re-run after changing rightkit-agent-tools or rightkit-automation.
# A stable identifier + Developer ID signature keeps the macOS Accessibility / Screen Recording grant
# valid across rebuilds.
#
# The binary signed and installed is the exact executable reported by the build this script just ran
# (cargo compiler-artifact JSON), never the newest file found under a workspace. The sha256 is checked
# before and after the copy so a changed or substituted file is refused.
set -eu
cd "$(dirname "$0")/.."
root="${RIGHTKIT_MANAGED_ROOT:-/Volumes/D/.rightkit-managed}"
dest="$root/agent-bin/right-ctl"
build_log="$(mktemp)"
trap 'rm -f "$build_log" "$dest.tmp"' EXIT

rustright cargo build -p rightkit-agent-tools --release --bin right-ctl --message-format=json > "$build_log"

# Collect the executable(s) cargo reported for the right-ctl bin target. Exactly one must exist.
bin="$(python3 -I -c '
import json, sys
exes = set()
for line in open(sys.argv[1]):
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get("reason") != "compiler-artifact":
        continue
    target = msg.get("target", {})
    if target.get("name") == "right-ctl" and "bin" in target.get("kind", []) and msg.get("executable"):
        exes.add(msg["executable"])
if len(exes) == 1:
    print(exes.pop())
elif len(exes) > 1:
    sys.stderr.write("install-right-ctl: build reported %d right-ctl executables; refusing\n" % len(exes))
' "$build_log")"
[ -n "$bin" ] || { echo "install-right-ctl: build did not report a right-ctl executable; refusing" >&2; exit 1; }
[ -f "$bin" ] || { echo "install-right-ctl: reported executable is missing: $bin" >&2; exit 1; }

src_sha="$(shasum -a 256 "$bin" | awk '{print $1}')"
cp "$bin" "$dest.tmp"
tmp_sha="$(shasum -a 256 "$dest.tmp" | awk '{print $1}')"
[ "$src_sha" = "$tmp_sha" ] || { echo "install-right-ctl: copy sha256 mismatch ($src_sha != $tmp_sha); refusing" >&2; exit 1; }

codesign --force --options runtime --timestamp --identifier com.orthiclabs.rightkit.right-ctl -s "Developer ID Application" "$dest.tmp"
mv "$dest.tmp" "$dest"
echo '[{"tool":"desktop.status","args":{}}]' | "$dest" run
echo "installed $dest from $bin (sha256 $src_sha before copy)"
