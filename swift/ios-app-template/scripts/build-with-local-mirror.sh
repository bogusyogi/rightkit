#!/bin/sh
# BUILD PROOF ONLY. Tag 0.3.0 is not published yet, so mirror ../RightKitSwift into a scratch git
# repo tagged with the pinned version and redirect the GitHub URL to it for this process only.
# project.yml is NOT modified; it keeps the git URL + exact tag.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
work=${MIRROR_WORK:-$(mktemp -d)}
ver=$(sed -n 's/^[[:space:]]*exactVersion: //p' "$here/project.yml")
"$here/scripts/check-package-pin.sh"
mkdir -p "$work/mirror"
cp -R "$here/../RightKitSwift/." "$work/mirror/"
rm -rf "$work/mirror/.build"
git -C "$work/mirror" init -q
git -C "$work/mirror" add -A
git -C "$work/mirror" -c user.name=mirror -c user.email=mirror@localhost commit -q --allow-empty -m "mirror $ver"
git -C "$work/mirror" tag -f "$ver"
printf '[url "file://%s/mirror"]\n\tinsteadOf = https://github.com/bogusyogi/rightkit-swift.git\n' "$work" > "$work/gitconfig"
export GIT_CONFIG_GLOBAL="$work/gitconfig"
cd "$here"
xcodegen generate --spec project.yml
xcodebuild build -project RightAppTemplate.xcodeproj -scheme RightAppTemplate \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath "$work/dd" \
  -clonedSourcePackagesDirPath "$work/spm" CODE_SIGNING_ALLOWED=NO
