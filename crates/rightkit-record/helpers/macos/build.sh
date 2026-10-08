#!/bin/sh
# Builds the rightkit-record macOS helper (and the off-screen fixture window used by live tests)
# in release mode and signs both ad hoc, which is enough to run them locally and in tests.
#
#   ./build.sh            build + ad-hoc sign; prints the helper path
#   ./build.sh test       also run the Swift unit tests (debug) and the timing test (release)
#
# Production: the release tooling signs rightkit-record-macos-helper with the Developer ID
# identity and notarizes it (see docs/rules/release-signing.md). This script does not, and an
# ad-hoc build must never ship. Screen-recording permission (TCC) is granted per signing
# identity and responsible process, so a differently signed helper starts with no grant.
#
# Use the result from Rust with RIGHTKIT_RECORD_MACOS_HELPER=<path printed below>.
set -eu
cd "$(dirname "$0")"

swift build -c release
BIN="$(swift build -c release --show-bin-path)"
codesign --force --sign - --identifier dev.rightkit.record.macos-helper "$BIN/rightkit-record-macos-helper"
codesign --force --sign - --identifier dev.rightkit.record.fixture-window "$BIN/rk-fixture-window"

if [ "${1:-}" = "test" ]; then
    swift test
    swift test -c release -Xswiftc -enable-testing
fi

echo "helper:  $BIN/rightkit-record-macos-helper"
echo "fixture: $BIN/rk-fixture-window"
