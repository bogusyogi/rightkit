#!/usr/bin/env bash
# Link probe for RightKitSpeech (Sage verdict 2026-10-24). Usage: run-probe.sh macos|ios
# Builds both executables of this fixture, then scans:
#   SpeechNoAppleProbe  must have no Speech.framework load command, no LC_LINKER_OPTION naming Speech in any object file of
#                       the speech-free modules (RightKitSpeech, RightKitAudio, RightKitArtifacts), and no SFSpeech /
#                       SpeechAnalyzer / SpeechTranscriber / AssetInventory / $s6Speech symbol.
#   Positive controls   AppleControlProbe must load Speech.framework and carry SFSpeech symbols; the fixture-local SpeechControl
#                       object files must name Speech. A control that finds nothing means the scanner is blind: fail.
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
    objects="$build/out/Intermediates.noindex"
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

# shellcheck disable=SC2016  # literal on purpose: the pattern matches the mangled symbol text, nothing is expanded
SYMBOLS='SFSpeech|SpeechAnalyzer|SpeechTranscriber|AssetInventory|\$s6Speech'
fail=0

# Load commands, then the linker options autolink leaves in object files, then symbols.
loads_speech() { otool -L "$1" | grep -q 'Speech\.framework'; }

# Object files of module $2 under $1: SwiftPM names them <Module>.build (executables) or <Module>-t.build (test targets).
# -L follows the macOS debug symlink, which BSD find otherwise treats as a leaf.
object_files() { find -L "$1" -name '*.o' \( -path "*/$2.build/*" -o -path "*/$2-t.build/*" \) 2>/dev/null || true; }
count_objects() { object_files "$1" "$2" | grep -c . || true; }

# True when an LC_LINKER_OPTION block in object $1 names Speech. Walks each block through all of its strings, not a fixed window.
names_speech() {
  otool -l "$1" 2>/dev/null | awk '
    /^ *cmd /              { inopt = ($2 == "LC_LINKER_OPTION") }
    inopt && /string #/ && /Speech/ { found = 1 }
    END { exit found ? 0 : 1 }'
}
# The object files of module $2 under $1 whose LC_LINKER_OPTION names Speech.
speech_objects() {
  local f
  object_files "$1" "$2" | while IFS= read -r f; do
    if names_speech "$f"; then printf '%s\n' "$f"; fi
  done || true
}

symbols_speech() { nm -m "$1" 2>/dev/null | grep -E "$SYMBOLS" || true; }

clean="$products/SpeechNoAppleProbe"
control="$products/AppleControlProbe"
# shellcheck disable=SC2015  # each A && B is a test, so the || branch runs exactly when a requirement is missing
[ -f "$clean" ] && [ -f "$control" ] || { echo "probe binaries missing under $products" >&2; exit 2; }

if loads_speech "$clean"; then echo "FAIL: SpeechNoAppleProbe loads Speech.framework"; fail=1; fi

# Object scan over every speech-free module. Zero object files for a module means the scan saw nothing: fail.
for mod in RightKitSpeech RightKitAudio RightKitArtifacts; do
  if [ "$(count_objects "$objects" "$mod")" -eq 0 ]; then
    echo "FAIL: object scan matched zero $mod object files under $objects (the scan is blind)"; fail=1; continue
  fi
  hits="$(speech_objects "$objects" "$mod")"
  if [ -n "$hits" ]; then echo "FAIL: LC_LINKER_OPTION naming Speech in $mod objects:"; echo "$hits"; fail=1; fi
done

syms="$(symbols_speech "$clean")"
if [ -n "$syms" ]; then echo "FAIL: Speech symbols in SpeechNoAppleProbe:"; echo "$syms" | head -20; fail=1; fi

# Positive controls.
if ! loads_speech "$control"; then echo "FAIL: control AppleControlProbe does not load Speech.framework (the scan is blind)"; fail=1; fi
if [ "$(count_objects "$objects" SpeechControl)" -eq 0 ]; then
  echo "FAIL: object control matched zero SpeechControl object files under $objects (the scan is blind)"; fail=1
elif [ -z "$(speech_objects "$objects" SpeechControl)" ]; then
  echo "FAIL: object control: no SpeechControl object names Speech (the scan is blind)"; fail=1
fi
sf="$(nm -m "$control" 2>/dev/null | grep 'SFSpeech' || true)"
if [ -z "$sf" ]; then echo "FAIL: symbol control: AppleControlProbe has no SFSpeech symbol (the scan is blind)"; fail=1; fi

[ "$fail" -eq 0 ] && echo "link probe ($platform): SpeechNoAppleProbe has no Speech; controls: AppleControlProbe has Speech.framework and SFSpeech symbols, SpeechControl objects name Speech"
exit "$fail"
