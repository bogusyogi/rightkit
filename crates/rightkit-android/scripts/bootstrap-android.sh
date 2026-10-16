#!/usr/bin/env sh
# Checks (default) or installs (--install) the pinned Android toolchain for RightKit builds.
#
# Idempotent. Reads pins from toolchain.versions next to this script. Check mode changes nothing
# and exits 1 when something is missing. --install adds what it safely can: rustup targets,
# cargo-ndk at the pinned version, and SDK packages through an EXISTING sdkmanager. It never accepts
# SDK licences, never downloads or runs an unpinned installer, and never edits shell profiles or
# system settings; it prints the variables to set. Android Studio (JBR, cmdline-tools) is installed
# by a person.
#
# Usage: bootstrap-android.sh [--install] [--sdk-root DIR]
# Run it through the owner chat only (rightkit#148). It has not been run by its author.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install=0
sdk_root=""
while [ $# -gt 0 ]; do
    case "$1" in
        --install) install=1 ;;
        --sdk-root) shift; sdk_root="${1:?--sdk-root needs a directory}" ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

pin() {
    value=$(grep -E "^$1=" "$here/toolchain.versions" | head -n 1 | cut -d= -f2-)
    if [ -z "$value" ]; then echo "toolchain.versions has no $1" >&2; exit 2; fi
    printf '%s' "$value"
}
NDK_VERSION=$(pin NDK_VERSION)
CARGO_NDK_VERSION=$(pin CARGO_NDK_VERSION)
PLATFORM_API=$(pin PLATFORM_API)
BUILD_TOOLS_VERSION=$(pin BUILD_TOOLS_VERSION)
RUST_TARGETS=$(pin RUST_TARGETS)
JDK_MAJOR=$(pin JDK_MAJOR)

missing=0
ok() { printf 'OK        %s\n' "$1"; }
miss() { printf 'MISSING   %s\n' "$1"; missing=$((missing + 1)); }
did() { printf 'INSTALLED %s\n' "$1"; }

os=$(uname -s)
if [ -z "$sdk_root" ]; then sdk_root="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"; fi
if [ -z "$sdk_root" ]; then
    case "$os" in
        Darwin) sdk_root="$HOME/Library/Android/sdk" ;;
        *) sdk_root="$HOME/Android/Sdk" ;;
    esac
fi
echo "SDK root: $sdk_root"

# ---- JDK (Android Studio JBR) -----------------------------------------------------------------
jdk="${JAVA_HOME:-}"
if [ -z "$jdk" ]; then
    for c in "/Applications/Android Studio.app/Contents/jbr/Contents/Home" \
             "/opt/android-studio/jbr" "$HOME/android-studio/jbr"; do
        if [ -x "$c/bin/java" ]; then jdk="$c"; break; fi
    done
fi
if [ -n "$jdk" ] && [ -x "$jdk/bin/java" ]; then
    major=""
    if [ -f "$jdk/release" ]; then
        major=$(sed -n 's/^JAVA_VERSION="\{0,1\}\([0-9][0-9]*\).*/\1/p' "$jdk/release" | head -n 1)
    fi
    if [ -n "$major" ] && [ "$major" -ge "$JDK_MAJOR" ]; then ok "JDK $major at $jdk"
    else miss "JDK $JDK_MAJOR+ (found: $jdk, major: ${major:-unknown}). Install Android Studio and use its JBR"; fi
else
    miss "JDK $JDK_MAJOR+ (set JAVA_HOME to Android Studio's jbr directory)"
fi

# ---- sdkmanager and licences -----------------------------------------------------------------
sdkmanager=""
if [ -d "$sdk_root/cmdline-tools" ]; then
    for d in $(ls -1 "$sdk_root/cmdline-tools" | sort -r); do
        if [ -x "$sdk_root/cmdline-tools/$d/bin/sdkmanager" ]; then
            sdkmanager="$sdk_root/cmdline-tools/$d/bin/sdkmanager"; break
        fi
    done
fi
if [ -n "$sdkmanager" ]; then ok "sdkmanager $sdkmanager"
else miss "sdkmanager (install 'Android SDK Command-line Tools' from Android Studio > SDK Manager)"; fi

licences_ok=0
if [ -f "$sdk_root/licenses/android-sdk-license" ]; then licences_ok=1; ok "SDK licences accepted"
else miss "SDK licences not accepted: run sdkmanager --sdk_root=\"$sdk_root\" --licenses yourself and read them"; fi

# ---- SDK packages ----------------------------------------------------------------------------
to_install=""
check_pkg() { # id dir
    if [ -d "$sdk_root/$2" ]; then ok "$1"
    else miss "$1"; to_install="$to_install $1"; fi
}
check_pkg "platforms;android-$PLATFORM_API" "platforms/android-$PLATFORM_API"
check_pkg "build-tools;$BUILD_TOOLS_VERSION" "build-tools/$BUILD_TOOLS_VERSION"
check_pkg "platform-tools" "platform-tools"
check_pkg "ndk;$NDK_VERSION" "ndk/$NDK_VERSION"
props="$sdk_root/ndk/$NDK_VERSION/source.properties"
if [ -f "$props" ] && ! grep -q "Pkg.Revision = $NDK_VERSION" "$props"; then
    miss "NDK at ndk/$NDK_VERSION does not report revision $NDK_VERSION (reinstall it)"
fi
if [ "$install" -eq 1 ] && [ -n "$to_install" ]; then
    if [ -n "$sdkmanager" ] && [ "$licences_ok" -eq 1 ]; then
        # shellcheck disable=SC2086
        "$sdkmanager" "--sdk_root=$sdk_root" $to_install
        for id in $to_install; do did "$id"; done
        missing=$((missing - $(echo $to_install | wc -w)))
    else
        echo "SKIPPED   SDK package install: needs sdkmanager and accepted licences (see above)"
    fi
fi

# ---- rustup targets ----------------------------------------------------------------------------
if command -v rustup >/dev/null 2>&1; then
    installed=$(rustup target list --installed)
    for t in $RUST_TARGETS; do
        if printf '%s\n' "$installed" | grep -qx "$t"; then ok "rustup target $t"
        elif [ "$install" -eq 1 ]; then rustup target add "$t"; did "rustup target $t"
        else miss "rustup target $t"; fi
    done
else
    miss "rustup (install Rust from rustup.rs; the workspace pins rust 1.98.0)"
fi

# ---- cargo-ndk ---------------------------------------------------------------------------------
have=""
if command -v cargo-ndk >/dev/null 2>&1; then have=$(cargo-ndk --version 2>&1 || true); fi
case "$have" in
    *"$CARGO_NDK_VERSION"*) ok "cargo-ndk $CARGO_NDK_VERSION" ;;
    *)
        if [ "$install" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
            cargo install cargo-ndk --version "$CARGO_NDK_VERSION" --locked
            did "cargo-ndk $CARGO_NDK_VERSION"
        else
            miss "cargo-ndk $CARGO_NDK_VERSION (found: ${have:-none})"
        fi ;;
esac

# ---- environment to set ----------------------------------------------------------------------
echo ""
echo "Set these in the shell that builds (this script does not change your environment):"
echo "  export ANDROID_HOME=\"$sdk_root\""
echo "  export ANDROID_NDK_HOME=\"$sdk_root/ndk/$NDK_VERSION\""
echo "  export NDK_HOME=\"\$ANDROID_NDK_HOME\""
if [ -n "$jdk" ]; then echo "  export JAVA_HOME=\"$jdk\""; fi

if [ "$missing" -gt 0 ]; then
    echo ""
    echo "$missing item(s) missing."
    exit 1
fi
echo "Android toolchain is complete."
