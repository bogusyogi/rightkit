#!/bin/bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: build.sh <bundled-node> <app-bundle-path>" >&2
  exit 2
fi

bundled_node="$1"
app_path="$2"
source_dir="$(cd "$(dirname "$0")" && pwd)"
release_tools="$source_dir/../../packages/release"
signer="$release_tools/sign-macos.mjs"
receipt_path="$app_path.signing-receipt.json"

if [[ -e "$app_path" || -e "$receipt_path" ]]; then
  echo "refusing to replace existing build output or signing receipt" >&2
  exit 1
fi
if [[ ! -x "$bundled_node" || ! -f "$signer" ]]; then
  echo "bundled Node or shared macOS signer is unavailable" >&2
  exit 1
fi

mkdir -p "$app_path/Contents/MacOS"
cp "$source_dir/Info.plist" "$app_path/Contents/Info.plist"
swiftc -parse-as-library -O -o "$app_path/Contents/MacOS/ownership-approval" "$source_dir/ownership-approval.swift"
"$bundled_node" "$signer" --receipt "$receipt_path" --entitlements "$source_dir/ownership-approval.entitlements" "$app_path"
"$app_path/Contents/MacOS/ownership-approval" --keychain-preflight
"$app_path/Contents/MacOS/ownership-approval" --crypto-self-test >/dev/null
"$app_path/Contents/MacOS/ownership-approval" --key-log-self-test >/dev/null
echo "RightKit ownership approval app: signed, preflighted, and software self-tests passed."
