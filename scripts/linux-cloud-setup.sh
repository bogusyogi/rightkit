#!/bin/bash
# One command that prepares a fresh Linux box (cloud VM, container, WSL2) to compile RightKit crates through the gate:
#   bash scripts/linux-cloud-setup.sh
# then:
#   node packages/release/bin/rightkit-gate.mjs --commit <sha> --to compile --units-file <file> --receipt-dir <new dir>
#
# Every step below is one that a real run on WSL2 (Ubuntu 24.04, 2026-11-10, commit 09aaf01e1) needed before the gate went green.
# Needs: bash, curl, tar, xz, git, apt-get, a C toolchain (build-essential), and this checkout. Downloads are pinned by sha256 and refused otherwise.
set -euo pipefail

NODE_VERSION="v26.12.1"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
DATA="${XDG_STATE_HOME:-$HOME/.local/state}/rightkit"
RELEASE="$REPO/packages/release"

# 0. System libraries the Tauri crates link against (rightkit-tauri-shell, rightkit-qa, the WebView consumer): without them
# gdk-sys, pango-sys, atk-sys and gdk-pixbuf-sys fail their build scripts. The list is the one CI installs
# (.github/workflows/publish-owner.yml, qualify-direct.yml).
SYSTEM_PACKAGES="pkg-config libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev"
MISSING_PACKAGES=""
for p in $SYSTEM_PACKAGES; do dpkg -s "$p" >/dev/null 2>&1 || MISSING_PACKAGES="$MISSING_PACKAGES $p"; done
if [ -n "$MISSING_PACKAGES" ]; then
  SUDO=""; [ "$(id -u)" = "0" ] || SUDO="sudo"
  $SUDO apt-get update -qq
  # shellcheck disable=SC2086
  DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq --no-install-recommends $MISSING_PACKAGES
fi

# 1. Node, through nvm, when the pinned version is not installed.
export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [ ! -x "$NVM_DIR/versions/node/$NODE_VERSION/bin/node" ]; then
  if [ ! -s "$NVM_DIR/nvm.sh" ]; then
    curl -fsSL -o /tmp/nvm-install.sh https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh
    echo "2d8359a64a3cb07c02389ad88ceecd43f2fa469c06104f92f98df5b6f315275f  /tmp/nvm-install.sh" | sha256sum -c - >/dev/null
    PROFILE=/dev/null bash /tmp/nvm-install.sh
  fi
  # shellcheck disable=SC1091
  . "$NVM_DIR/nvm.sh"
  nvm install "$NODE_VERSION"
fi
export PATH="$NVM_DIR/versions/node/$NODE_VERSION/bin:$HOME/.cargo/bin:$PATH"

# 2. The setup command imports from node_modules (smol-toml), so an empty checkout needs pnpm and one install before it can start.
# The pinned pnpm is installed into the pinned Node itself: checked from / so another pnpm on PATH that delegates by the repo's
# packageManager field does not pass for it (the gate's import probe runs pnpm outside the checkout and refuses a different one).
PNPM_VERSION="$(node -e 'console.log(/^pnpm@(\d+\.\d+\.\d+)/.exec(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).packageManager)[1])' "$REPO/package.json")"
[ "$(cd / && pnpm --version 2>/dev/null || true)" = "$PNPM_VERSION" ] || npm install -g "pnpm@$PNPM_VERSION"
if [ ! -d "$RELEASE/node_modules/smol-toml" ]; then
  (cd "$REPO" && pnpm install --frozen-lockfile)
fi

# 2b. rustup with the pinned toolchain, and the pnpm store (the existing setup command).
node "$RELEASE/bin/rightkit-linux-setup.mjs" --root "$REPO"

# 3. The other operating systems' targets: the gate lints every crate for macOS and Windows from Linux.
rustup target add aarch64-apple-darwin x86_64-pc-windows-msvc x86_64-pc-windows-gnu

# 4. Scan tools the quick checks require, each pinned by archive and binary sha256, then enrolled for this host.
enrol() { # tool version path; a tool already enrolled with this binary is left alone, so the script can be run again
  local sha; sha="$(sha256sum "$3" | cut -c1-64)"
  if node "$RELEASE/scan-tool-enrolment.mjs" show --platform linux --data-root "$DATA" 2>/dev/null | node -e 'const t=JSON.parse(require("fs").readFileSync(0,"utf8")).tools?.[process.argv[1]];process.exit(t&&t.sha256===process.argv[2]?0:1)' "$1" "$sha" 2>/dev/null; then return 0; fi
  node "$RELEASE/scan-tool-enrolment.mjs" enroll --platform linux --data-root "$DATA" --tool "$1" --path "$3" --sha256 "$(sha256sum "$3" | cut -c1-64)" --version "$2"
}
fetch_tool() { # tool version url archive-sha256 binary-sha256 binary-name
  local tool=$1 ver=$2 url=$3 asha=$4 bsha=$5 bin=$6 work dest found
  dest="$DATA/tools/$tool-$ver"
  if [ ! -x "$dest/$bin" ]; then
    work="$(mktemp -d)"
    curl -fsSL -o "$work/archive" "$url"
    echo "$asha  $work/archive" | sha256sum -c - >/dev/null
    mkdir "$work/x"
    tar -xf "$work/archive" -C "$work/x"
    found="$(find "$work/x" -type f -name "$bin" | head -1)"
    [ -n "$found" ] || { echo "no $bin in the $tool archive" >&2; exit 4; }
    echo "$bsha  $found" | sha256sum -c - >/dev/null
    mkdir -p "$dest"
    install -m 0755 "$found" "$dest/$bin"
    rm -rf "$work"
  fi
  enrol "$tool" "$ver" "$dest/$bin"
}
fetch_tool shellcheck 0.11.0 https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.linux.x86_64.tar.xz \
  8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 4da528ddb3a4d1b7b24a59d4e16eb2f5fd960f4bd9a3708a15baddbdf1d5a55b shellcheck
fetch_tool cargo-deny 0.20.2 https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz \
  9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f b329e25933d01c36dd7c47d84ea5716694f9b7caf53a5003d45674703a8ed54a cargo-deny
fetch_tool cargo-audit 0.22.2 https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-musl-v0.22.2.tgz \
  7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428 66f9c8f530950d106c1869ce27ef5a4008bcea53c7716c3ed8117586337ee7d8 cargo-audit

# 5. actionlint, pinned in ci/actionlint.json and installed by the repository's own script.
mkdir -p "$DATA/tools/actionlint"
RUNNER_TEMP="$DATA/tools/actionlint" node "$REPO/scripts/ci/provision-actionlint.mjs"
enrol actionlint "$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).version)' "$REPO/ci/actionlint.json")" "$DATA/tools/actionlint/actionlint-bin/actionlint"

# 6. The zig C toolchain the cross-target lint needs for crates with C code.
node "$REPO/scripts/ci/provision-zig.mjs" "$DATA/zig"

node "$RELEASE/scan-tool-enrolment.mjs" show --platform linux --data-root "$DATA" | cut -c1-600
echo "linux-cloud-setup: done. Put $NVM_DIR/versions/node/$NODE_VERSION/bin and $HOME/.cargo/bin on PATH, then run the gate."
