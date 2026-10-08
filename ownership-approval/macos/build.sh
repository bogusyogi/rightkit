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
impostor_path="$app_path.keychain-storage-impostor"

if [[ -e "$app_path" || -e "$receipt_path" || -e "$impostor_path" ]]; then
  echo "refusing to replace existing build output or signing receipt" >&2
  exit 1
fi
trap 'rm -f "$impostor_path"' EXIT
if [[ ! -x "$bundled_node" || ! -f "$signer" ]]; then
  echo "bundled Node or shared macOS signer is unavailable" >&2
  exit 1
fi
mkdir -p "$app_path/Contents/MacOS"
cp "$source_dir/Info.plist" "$app_path/Contents/Info.plist"
swiftc -parse-as-library -O -o "$app_path/Contents/MacOS/ownership-approval" "$source_dir/ownership-approval.swift"
swiftc -O -o "$impostor_path" "$source_dir/keychain-storage-impostor.swift"
"$bundled_node" "$signer" --receipt "$receipt_path" --entitlements "$source_dir/ownership-approval.entitlements" "$app_path"
preflight="$("$app_path/Contents/MacOS/ownership-approval" --keychain-preflight)"
if [[ "$preflight" != "Keychain preflight: PASS ("* ]]; then
  echo "native Keychain preflight did not confirm access" >&2
  exit 1
fi
printf '%s\n' "$preflight"
storage_test="$("$app_path/Contents/MacOS/ownership-approval" --keychain-storage-self-test "$impostor_path")"
if [[ "$storage_test" != "Keychain storage self-test: PASS ("* ]]; then
  echo "native signed-helper Keychain ACL self-test did not affirm access and denial" >&2
  exit 1
fi
printf '%s\n' "$storage_test"
"$app_path/Contents/MacOS/ownership-approval" --crypto-self-test >/dev/null
"$app_path/Contents/MacOS/ownership-approval" --key-log-self-test >/dev/null
echo "RightKit ownership approval app: signed, preflighted, and software self-tests passed."
