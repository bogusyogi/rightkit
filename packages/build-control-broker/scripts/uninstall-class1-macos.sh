#!/bin/sh
# Class-1 broker rollback (macOS, operator script). Dry-run by default; --apply as root executes.
# Removes the LaunchDaemons and the pinned node. It never deletes data roots, build roots, logs or receipts.
# --remove-identity also deletes the _rightkit account (only after the daemons are gone).
# --restore-agent <plist> re-bootstraps a previous per-user LaunchAgent for the console user.
set -eu

MODE=dry-run
LABEL=com.rightkit.build-broker
WATCHDOG_LABEL=com.rightkit.build-broker.watchdog
SERVICE_USER=_rightkit
CONTROL_ROOT="/Library/Application Support/RightKit"
DAEMON_DIR="/Library/LaunchDaemons"
REMOVE_IDENTITY=0
REMOVE_RUNTIME=0
RESTORE_AGENT=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --apply) MODE=apply ;;
    --dry-run) MODE=dry-run ;;
    --remove-identity) REMOVE_IDENTITY=1 ;;
    --remove-runtime) REMOVE_RUNTIME=1 ;;
    --restore-agent) RESTORE_AGENT="$2"; shift ;;
    --control-root) CONTROL_ROOT="$2"; shift ;;
    -h|--help) echo "usage: uninstall-class1-macos.sh [--dry-run|--apply] [--remove-runtime] [--remove-identity] [--restore-agent <plist>]"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
run() {
  if [ "$MODE" = apply ]; then say "  run: $*"; "$@"; else say "  would run: $*"; fi
}
fail() { say "refused: $*" >&2; exit 2; }

[ "$(uname -s)" = Darwin ] || fail "macOS only"
if [ "$MODE" = apply ] && [ "$(id -u)" -ne 0 ]; then fail "--apply must run as root"; fi
if [ -n "$RESTORE_AGENT" ]; then
  case "$RESTORE_AGENT" in /*) ;; *) fail "--restore-agent must be an absolute path" ;; esac
  [ -f "$RESTORE_AGENT" ] || fail "restore plist is missing: $RESTORE_AGENT"
fi
say "mode: $MODE"

say ""; say "STEP 1: stop and unload the daemons"
run sh -c "launchctl bootout system/$WATCHDOG_LABEL 2>/dev/null || true"
run sh -c "launchctl bootout system/$LABEL 2>/dev/null || true"

say ""; say "STEP 2: remove the LaunchDaemon plists"
run rm -f "$DAEMON_DIR/$WATCHDOG_LABEL.plist" "$DAEMON_DIR/$LABEL.plist"

if [ "$REMOVE_RUNTIME" = 1 ]; then
  say ""; say "STEP 3: remove the pinned node runtime"
  run rm -rf "$CONTROL_ROOT/runtime/node"
else
  say ""; say "STEP 3: keep the pinned node runtime (pass --remove-runtime to delete it)"
fi

if [ "$REMOVE_IDENTITY" = 1 ]; then
  say ""; say "STEP 4: remove the $SERVICE_USER identity (daemons must be gone first)"
  run sh -c "dscl . -delete /Users/$SERVICE_USER 2>/dev/null || true"
  run sh -c "dscl . -delete /Groups/$SERVICE_USER 2>/dev/null || true"
else
  say ""; say "STEP 4: keep the $SERVICE_USER identity (pass --remove-identity to delete it)"
fi

if [ -n "$RESTORE_AGENT" ]; then
  say ""; say "STEP 5: restore the previous LaunchAgent for the console user"
  console_uid=$(stat -f %u /dev/console 2>/dev/null || echo "")
  [ -n "$console_uid" ] || fail "cannot determine the console user"
  run launchctl bootstrap "gui/$console_uid" "$RESTORE_AGENT"
fi

say ""
say "STEP 6: data roots, build roots, logs and receipts are NOT touched by this script"
say ""
if [ "$MODE" = apply ]; then say "rollback applied."; else say "dry-run complete; nothing was changed."; fi
