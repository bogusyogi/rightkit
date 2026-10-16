#!/usr/bin/env sh
# Builds librightkit_android.so per ABI with cargo-ndk and generates the Kotlin bindings.
#
# Writes android/src/main/jniLibs/<abi>/ and android/src/main/uniffi-generated/ (both git-ignored)
# and a BUILD-RECEIPT.txt. Runs cargo through $RIGHTKIT_CARGO when set (for example "rightkit cargo"),
# otherwise "cargo". Run it through the owner chat only. It has not been run by its author.
#
# Usage: build-android.sh [--debug] [--crate PACKAGE] [--library LIBNAME]
#   --crate/--library: an app with its own cdylib that links rightkit-android passes its package
#   and its library name (default rightkit-android / rightkit_android).

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$here/.." && pwd)
root=$(CDPATH= cd -- "$crate_dir/../.." && pwd)
# One Cargo profile everywhere: the built-in dev profile (never --release). --debug is accepted for compatibility.
profile=""; profile_name=dev
crate="rightkit-android"; library="rightkit_android"
while [ $# -gt 0 ]; do
    case "$1" in
        --debug) ;;
        --crate) shift; crate="${1:?--crate needs a package}" ;;
        --library) shift; library="${1:?--library needs a name}" ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

pin() { grep -E "^$1=" "$here/toolchain.versions" | head -n 1 | cut -d= -f2-; }
abis=$(pin ABIS); min_sdk=$(pin MIN_SDK); ndk=$(pin NDK_VERSION)
cargo_cmd="${RIGHTKIT_CARGO:-cargo}"

"$here/bootstrap-android.sh" || { echo "toolchain check failed; fix it or run bootstrap-android.sh --install" >&2; exit 1; }

jni="$crate_dir/android/src/main/jniLibs"
gen="$crate_dir/android/src/main/uniffi-generated"
targets=""
for abi in $abis; do targets="$targets -t $abi"; done

cd "$root"
# shellcheck disable=SC2086
$cargo_cmd ndk $targets --platform "$min_sdk" -o "$jni" build -p "$crate" $profile

rm -rf "$gen"
# shellcheck disable=SC2086
$cargo_cmd run -p rightkit-android --features bindgen --bin uniffi-bindgen -- \
    generate --library "$jni/arm64-v8a/lib$library.so" --language kotlin --out-dir "$gen" --no-format

{
    echo "built:   $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "git:     $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "crate:   $crate ($profile_name)"
    echo "ndk:     $ndk"
    echo "minSdk:  $min_sdk"
    echo "abis:    $abis"
    for f in "$jni"/*/*.so; do
        if command -v sha256sum >/dev/null 2>&1; then sum=$(sha256sum "$f" | cut -d' ' -f1)
        else sum=$(shasum -a 256 "$f" | cut -d' ' -f1); fi
        echo "sha256:  $sum  ${f#"$jni"/}"
    done
} > "$jni/BUILD-RECEIPT.txt"
echo "Done. Receipt: $jni/BUILD-RECEIPT.txt"
