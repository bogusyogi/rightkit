#!/bin/bash
# Reproducible arm64 smartctl build (smartmontools, GPL-2.0-or-later) for the RightKit native
# artifact catalog. Ships as a separate executable (Pulse.app/Contents/Helpers/smartctl); it is
# never linked into an app.
#
#   build-macos.sh build --source <smartmontools-7.5.tar.gz> --work <empty dir> --out <dir>
#       Verify the pinned source sha256, configure minimally, build smartctl only, strip, and
#       write <out>/smartctl (UNSIGNED) plus <out>/smartctl.evidence.json. Signing is separate:
#       node packages/release/sign-macos.mjs --receipt <receipt.json> <out>/smartctl
#
# Pin: smartmontools 7.5, https://github.com/smartmontools/smartmontools/releases/download/RELEASE_7_5/smartmontools-7.5.tar.gz
# Observed sha256 equals nothing upstream publishes directly; upstream publishes md5 + OpenPGP .asc,
# and the md5 below matches both GitHub's .md5 asset and SourceForge's file listing.
set -euo pipefail

VERSION=7.5
SOURCE_SHA256=690b83ca331378da9ea0d9d61008c4b22dde391387b9bbad7f29387f2595f76e
SOURCE_MD5=38c38b0b82db7fc4906cdd50d15a7931
DEPLOYMENT_TARGET=14.0
ARCH=arm64

[ "${1:-}" = build ] || { echo "usage: $0 build --source <tar.gz> --work <dir> --out <dir>" >&2; exit 2; }
shift
source="" work="" out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --source) source="$2"; shift 2 ;;
    --work) work="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
# shellcheck disable=SC2015  # each A && B is a test, so the || branch runs exactly when a requirement is missing
[ -n "$source" ] && [ -n "$work" ] && [ -n "$out" ] || { echo "--source, --work and --out are required" >&2; exit 2; }
[ "$(uname -s)-$(uname -m)" = Darwin-arm64 ] || { echo "build on an arm64 Mac" >&2; exit 1; }

got="$(shasum -a 256 "$source" | cut -d' ' -f1)"
[ "$got" = "$SOURCE_SHA256" ] || { echo "source sha256 mismatch: $got" >&2; exit 1; }
[ "$(md5 -q "$source")" = "$SOURCE_MD5" ] || { echo "source md5 mismatch" >&2; exit 1; }

mkdir -p "$work" "$out"
[ -z "$(ls -A "$work")" ] || { echo "--work must be empty" >&2; exit 1; }
tar -xzf "$source" -C "$work"
src="$work/smartmontools-$VERSION"

export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"
export CC="clang -arch $ARCH" CXX="clang++ -arch $ARCH"
export CFLAGS="-O2 -mmacosx-version-min=$DEPLOYMENT_TARGET" CXXFLAGS="-O2 -mmacosx-version-min=$DEPLOYMENT_TARGET"
export LDFLAGS="-mmacosx-version-min=$DEPLOYMENT_TARGET"
export SOURCE_DATE_EPOCH=1745998691 TZ=UTC LC_ALL=C
CONFIGURE_FLAGS=(--host=aarch64-apple-darwin --disable-dependency-tracking
  --without-gnupg --without-selinux --without-libcap-ng --without-libsystemd
  --without-update-smart-drivedb --without-initscriptdir --without-systemdsystemunitdir)

cd "$src"
./configure "${CONFIGURE_FLAGS[@]}" >"$out/configure.log" 2>&1
make -j"$(sysctl -n hw.ncpu)" smartctl >"$out/make.log" 2>&1
cp smartctl "$out/smartctl"
strip -x "$out/smartctl"
chmod 0755 "$out/smartctl"

bad="$(otool -L "$out/smartctl" | tail -n +2 | awk '{print $1}' | grep -v -E '^(/usr/lib/|/System/)' || true)"
[ -z "$bad" ] || { echo "non-system import: $bad" >&2; exit 1; }

python3 - "$out" "$VERSION" "$SOURCE_SHA256" "$DEPLOYMENT_TARGET" "${CONFIGURE_FLAGS[*]}" <<'PY'
import hashlib, json, subprocess, sys
out, version, src_sha, target, flags = sys.argv[1:6]
b = open(f"{out}/smartctl", "rb").read()
sh = lambda *a: subprocess.run(a, capture_output=True, text=True).stdout.strip()
json.dump({
  "tool": "smartctl", "version": version, "arch": "arm64",
  "sha256Unsigned": hashlib.sha256(b).hexdigest(), "size": len(b),
  "sourceSha256": src_sha, "deploymentTarget": target,
  "compiler": sh("clang", "--version").splitlines()[0], "sdk": sh("xcrun", "--show-sdk-version"),
  "configure": flags, "cflags": "-O2 -mmacosx-version-min=" + target,
  "otool": sh("otool", "-L", f"{out}/smartctl").splitlines()[1:],
  "minos": sh("otool", "-l", f"{out}/smartctl"),
}, open(f"{out}/smartctl.evidence.json", "w"), indent=2)
PY
echo "built $out/smartctl"
