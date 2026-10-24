#!/usr/bin/env bash
# Link probe for RightKitSpeech (Sage verdict 2026-10-24). Usage: run-probe.sh macos|ios
# Builds both executables of this fixture, then scans:
#   SpeechNoAppleProbe  must have no Speech.framework load command, no LC_LINKER_OPTION naming Speech in any object file,
#                       and no SFSpeech / SpeechAnalyzer / SpeechTranscriber / AssetInventory / $s6Speech symbol.
#   AppleControlProbe   positive control: the same scan MUST find Speech.framework, or the scanner is blind.
set -euo pipefail
platform="${1:?usage: run-probe.sh macos|ios}"
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"
build="${PROBE_BUILD_DIR:-/Volumes/D/scratch/speech-link-probe-$platform}"
rm -rf "$build"; mkdir -p "$build"

case "$platform" in
  macos)
    swift build --scratch-path "$build"
    products="$build/debug"
    objects="$build/debug"
    ;;
  ios)
    for scheme in SpeechNoAppleProbe AppleControlProbe; do
      xcodebuild build -scheme "$scheme" -destination generic/platform=iOS -derivedDataPath "$build" \
        -skipPackagePluginValidation CODE_SIGNING_ALLOWED=NO
    done
    products="$build/Build/Products/Debug-iphoneos"
    objects="$build/Build/Intermediates.noindex"
    ;;
  *) echo "unknown platform $platform" >&2; exit 2 ;;
esac

SYMBOLS='SFSpeech|SpeechAnalyzer|SpeechTranscriber|AssetInventory|\$s6Speech'
fail=0

# Load commands, then the linker options autolink leaves in object files, then symbols.
loads_speech() { otool -L "$1" | grep -q 'Speech\.framework'; }
objects_name_speech() {
  # swiftc records an autolink request as an LC_LINKER_OPTION (or a .swift1_autolink_entries section on ELF-style output).
  find "$1" -name '*.o' -path "*$2*" -print0 | xargs -0 -I{} sh -c \
    "otool -l '{}' 2>/dev/null | grep -A3 LC_LINKER_OPTION | grep -q 'Speech' && echo '{}'" || true
}
symbols_speech() { nm -m "$1" 2>/dev/null | grep -E "$SYMBOLS" || true; }

clean="$products/SpeechNoAppleProbe"
control="$products/AppleControlProbe"
[ -f "$clean" ] && [ -f "$control" ] || { echo "probe binaries missing under $products" >&2; exit 2; }

if loads_speech "$clean"; then echo "FAIL: SpeechNoAppleProbe loads Speech.framework"; fail=1; fi
hits="$(objects_name_speech "$objects" RightKitSpeech.build)"
if [ -n "$hits" ]; then echo "FAIL: LC_LINKER_OPTION naming Speech in RightKitSpeech objects:"; echo "$hits"; fail=1; fi
syms="$(symbols_speech "$clean")"
if [ -n "$syms" ]; then echo "FAIL: Speech symbols in SpeechNoAppleProbe:"; echo "$syms" | head -20; fail=1; fi

# Positive control.
if ! loads_speech "$control"; then echo "FAIL: control AppleControlProbe does not load Speech.framework (the scan is blind)"; fail=1; fi

[ "$fail" -eq 0 ] && echo "link probe ($platform): SpeechNoAppleProbe has no Speech; control AppleControlProbe has Speech.framework"
exit "$fail"
