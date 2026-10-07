#!/bin/sh
# Build, Developer-ID-sign and install `right-ctl` (rightkit-agent-tools) into the managed agent-bin,
# which every agent shell has on PATH. Re-run after changing rightkit-agent-tools or rightkit-automation.
# A stable identifier + Developer ID signature keeps the macOS Accessibility / Screen Recording grant
# valid across rebuilds.
set -eu
cd "$(dirname "$0")/.."
root="${RIGHTKIT_MANAGED_ROOT:-/Volumes/D/.rightkit-managed}"
dest="$root/agent-bin/right-ctl"
rightkit cargo build -p rightkit-agent-tools --release --bin right-ctl
bin="$(find "$root/rightkit-build-control/ws" -path '*/release/right-ctl' -type f -print0 | xargs -0 ls -t | head -1)"
[ -n "$bin" ] || { echo "install-right-ctl: built binary not found under $root" >&2; exit 1; }
cp "$bin" "$dest.tmp"
codesign --force --options runtime --timestamp --identifier com.orthiclabs.rightkit.right-ctl -s "Developer ID Application" "$dest.tmp"
mv "$dest.tmp" "$dest"
echo '[{"tool":"desktop.status","args":{}}]' | "$dest" run
echo "installed $dest"
