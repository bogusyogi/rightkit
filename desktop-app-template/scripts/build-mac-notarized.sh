#!/usr/bin/env bash
# macOS Developer ID build for right-release (signing contract macos-developer-id-notarized-portable-v1).
#   --prepare-right-release   build, deep-sign, notarize + staple the app, build the DMG, mirror it to the repo root
#   --finalize-right-release  sign, notarize and staple the prepared DMG (run by `rightkit:package:mac`)
# Template: RightApp/rightapp are replaced by `rightapp-tools init`. Never run outside the native Mac host.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="RightApp"
BIN_NAME="rightapp"
TARGET_TRIPLE="aarch64-apple-darwin"
ARCH_LABEL="aarch64" # matches the DMG name in right-release.config.mjs
NOTARY_PROFILE="${NOTARY_KEYCHAIN_PROFILE:-apple-dev-notary}"
# Identity comes from the environment or the keychain; nothing account-specific is hard-coded here.
SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"

MODE="${1:-}"
case "$MODE" in
  --prepare-right-release|--finalize-right-release) ;;
  *) echo "usage: $0 --prepare-right-release|--finalize-right-release" >&2; exit 2 ;;
esac

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS build/notarization must run on macOS." >&2
  exit 1
fi

if [[ -z "$SIGNING_IDENTITY" ]]; then
  SIGNING_IDENTITY="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -n 1)"
fi
if [[ -z "$SIGNING_IDENTITY" ]] || ! security find-identity -v -p codesigning | grep -Fq "$SIGNING_IDENTITY"; then
  echo "Missing Developer ID Application signing identity (set APPLE_SIGNING_IDENTITY or install the certificate)." >&2
  exit 1
fi
if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null; then
  echo "Missing or invalid notarytool keychain profile: $NOTARY_PROFILE" >&2
  exit 1
fi

VERSION="$(node -p "require('./package.json').version")"
DMG_ROOT="$ROOT/${APP_NAME}_${VERSION}_${ARCH_LABEL}.dmg"

if [[ "$MODE" == "--finalize-right-release" ]]; then
  # right-release signs the prepared DMG (sign.prePackageFiles) before this step.
  codesign --verify --strict --verbose=2 "$DMG_ROOT"
  xcrun notarytool submit "$DMG_ROOT" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG_ROOT"
  xcrun stapler validate "$DMG_ROOT"
  hdiutil verify "$DMG_ROOT"
  exit 0
fi

# Managed builds own the Cargo target root; resolve it from cargo metadata, never from env.
CARGO_TARGET_ROOT="$(node --input-type=module -e "import { resolveTargetRoot } from '@rightkit/release/cargo-target.mjs'; console.log(resolveTargetRoot(process.argv[1]));" "$ROOT/Cargo.toml")"
APP_BUNDLE_DIR="$CARGO_TARGET_ROOT/$TARGET_TRIPLE/release/bundle/macos"
APP_PATH="$APP_BUNDLE_DIR/${APP_NAME}.app"
APP_ZIP="$APP_BUNDLE_DIR/${APP_NAME}.zip"
DMG_DIR="$CARGO_TARGET_ROOT/$TARGET_TRIPLE/release/bundle/dmg"
DMG_PATH="$DMG_DIR/${APP_NAME}_${VERSION}_${ARCH_LABEL}.dmg"

rm -rf "$APP_PATH" "$APP_ZIP" "$DMG_DIR"
pnpm exec tauri build --target "$TARGET_TRIPLE" --bundles app --config '{"bundle":{"createUpdaterArtifacts":false}}'

if [[ ! -d "$APP_PATH" ]]; then
  echo "Expected app bundle was not produced: $APP_PATH" >&2
  exit 1
fi

while IFS= read -r -d '' file; do
  if file -b "$file" | grep -q "Mach-O"; then
    codesign --force --sign "$SIGNING_IDENTITY" --options runtime --timestamp "$file"
  fi
done < <(find "$APP_PATH" -type f -print0)
codesign --force --sign "$SIGNING_IDENTITY" --options runtime --timestamp "$APP_PATH"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

ditto -c -k --keepParent "$APP_PATH" "$APP_ZIP"
xcrun notarytool submit "$APP_ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"
spctl --assess --type execute --verbose "$APP_PATH"

# Plain DMG (app + /Applications link). Apps that want a branded layout add Tauri's bundle_dmg.sh here.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ditto "$APP_PATH" "$STAGE/${APP_NAME}.app"
ln -s /Applications "$STAGE/Applications"
mkdir -p "$DMG_DIR"
rm -f "$DMG_PATH"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH"

pnpm exec right-release mirror-root-artifact --file "$DMG_PATH" --package-root "$ROOT"
rm -rf "$APP_PATH"
pnpm exec right-release lsclean "${APP_NAME}.app"
echo "DMG prepared: $DMG_PATH (finalized by --finalize-right-release)"
