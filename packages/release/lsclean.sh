#!/bin/sh
# lsclean.sh — drop a built macOS app from LaunchServices (Launchpad/Spotlight).
#
# macOS auto-registers every .app bundle it discovers, so each build of a Right
# Suite app makes a build-folder copy show up in Launchpad even though it isn't
# installed in /Applications. This unregisters every registered copy of <Name.app>
# that is NOT under /Applications (i.e. build folders, .Trash, stale mounts).
#
# Usage:   lsclean.sh HeardRight.app
#          lsclean.sh ViewRight.app
# Run it after a build, or any time Launchpad shows phantom copies.
# Note: `lsregister -kill` was removed by Apple; per-path `-u` is the replacement.

set -eu
LSR=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
name="${1:?usage: lsclean.sh <AppName.app>}"

"$LSR" -dump \
  | sed -n 's/^[[:space:]]*path:[[:space:]]*\(.*'"$name"'\) (0x[0-9a-f]*)$/\1/p' \
  | sort -u \
  | while IFS= read -r p; do
      case "$p" in
        /Applications/*) ;;                    # keep real installs
        *) "$LSR" -u "$p" 2>/dev/null && echo "unregistered: $p" ;;
      esac
    done

echo "done. (a fresh build re-registers the build-folder copy — re-run then.)"
