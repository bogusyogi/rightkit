#!/bin/sh
# Class-1 broker install for macOS (operator script).
# Dry-run by default: prints every step and the exact command it would run.
# Pass --apply, as root (sudo), to execute. Idempotent: re-running converges.
# This script never runs sudo itself and never touches ~/Library or live data roots beyond chown/chmod.
set -eu

MODE=dry-run
LABEL=com.rightkit.build-broker
WATCHDOG_LABEL=com.rightkit.build-broker.watchdog
SERVICE_USER=_rightkit
CONTROL_ROOT="/Library/Application Support/RightKit"
NODE_SRC=""
BROKER_ENTRY=""
BROKER_CONFIG=""
WATCHDOG_ENTRY=""
CARGO_HOME_ROOT=""
GIT_DB_ROOT=""
SCCACHE_ROOT=""
BUILD_ROOT_PARENT=""
SOCKET=""
LIFECYCLE=""
DAEMON_DIR="/Library/LaunchDaemons"

usage() {
  cat <<'USAGE'
usage: install-class1-macos.sh [--dry-run|--apply] --node <abs node binary> --broker-entry <abs broker.mjs>
         --broker-config <abs config.json> --watchdog-entry <abs watchdog.mjs>
         --cargo-home <dir> --git-db <dir> --sccache <dir> --build-root-parent <dir>
         [--socket <abs path>] [--lifecycle <abs lifecycle.ndjson>] [--control-root <dir>]
Dry-run is the default. --apply requires root (run the whole command with sudo yourself).
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --apply) MODE=apply ;;
    --dry-run) MODE=dry-run ;;
    --node) NODE_SRC="$2"; shift ;;
    --broker-entry) BROKER_ENTRY="$2"; shift ;;
    --broker-config) BROKER_CONFIG="$2"; shift ;;
    --watchdog-entry) WATCHDOG_ENTRY="$2"; shift ;;
    --cargo-home) CARGO_HOME_ROOT="$2"; shift ;;
    --git-db) GIT_DB_ROOT="$2"; shift ;;
    --sccache) SCCACHE_ROOT="$2"; shift ;;
    --build-root-parent) BUILD_ROOT_PARENT="$2"; shift ;;
    --socket) SOCKET="$2"; shift ;;
    --lifecycle) LIFECYCLE="$2"; shift ;;
    --control-root) CONTROL_ROOT="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

[ -n "$SOCKET" ] || SOCKET="$CONTROL_ROOT/run/broker.sock"
[ -n "$LIFECYCLE" ] || LIFECYCLE="$CONTROL_ROOT/logs/lifecycle.ndjson"
PLIST="$DAEMON_DIR/$LABEL.plist"
WATCHDOG_PLIST="$DAEMON_DIR/$WATCHDOG_LABEL.plist"

say() { printf '%s\n' "$*"; }
step() { say ""; say "STEP $1: $2"; }

# Every mutating command goes through run(): printed always, executed only with --apply.
run() {
  if [ "$MODE" = apply ]; then
    say "  run: $*"
    "$@"
  else
    say "  would run: $*"
  fi
}

fail() { say "refused: $*" >&2; exit 2; }

# ---- preflight (read-only, always executed) -------------------------------
step 0 "preflight (read-only)"
[ "$(uname -s)" = Darwin ] || fail "this installer is macOS only"
[ -x /usr/bin/sandbox-exec ] || fail "/usr/bin/sandbox-exec is required for class-1 children"
for required in NODE_SRC BROKER_ENTRY BROKER_CONFIG WATCHDOG_ENTRY CARGO_HOME_ROOT GIT_DB_ROOT SCCACHE_ROOT BUILD_ROOT_PARENT; do
  eval "value=\$$required"
  [ -n "$value" ] || fail "--$(printf '%s' "$required" | tr 'A-Z_' 'a-z-') is required"
done
for path in "$NODE_SRC" "$BROKER_ENTRY" "$BROKER_CONFIG" "$WATCHDOG_ENTRY" "$CARGO_HOME_ROOT" "$GIT_DB_ROOT" "$SCCACHE_ROOT" "$BUILD_ROOT_PARENT" "$CONTROL_ROOT" "$SOCKET" "$LIFECYCLE"; do
  case "$path" in /*) ;; *) fail "paths must be absolute: $path" ;; esac
  case "$path" in *[\"\<\>\&\']*) fail "path contains characters that cannot be placed in a plist: $path" ;; esac
done
case "$NODE_SRC" in
  "$HOME"/*|/Users/*/.nvm/*|*/.nvm/*) fail "node must be pinned from outside a user home (got $NODE_SRC); copy a system or signed node first" ;;
esac
[ -f "$NODE_SRC" ] && [ -x "$NODE_SRC" ] || fail "node binary is not an executable file: $NODE_SRC"
[ -f "$BROKER_ENTRY" ] || fail "broker entry is missing: $BROKER_ENTRY"
[ -f "$WATCHDOG_ENTRY" ] || fail "watchdog entry is missing: $WATCHDOG_ENTRY"
[ -f "$BROKER_CONFIG" ] || fail "broker config is missing: $BROKER_CONFIG"
# clonefile needs one APFS volume: every build root parent must share the device of each source data root.
build_dev=$(stat -f %d "$BUILD_ROOT_PARENT" 2>/dev/null) || fail "build root parent does not exist: $BUILD_ROOT_PARENT"
for root in "$CARGO_HOME_ROOT" "$GIT_DB_ROOT" "$SCCACHE_ROOT"; do
  [ -d "$root" ] || fail "data root does not exist: $root"
  [ "$(stat -f %d "$root")" = "$build_dev" ] || fail "data root $root is on a different volume than the build root parent (clonefile requires one volume)"
done
say "  node source: $NODE_SRC ($(shasum -a 256 "$NODE_SRC" | awk '{print $1}'))"
say "  control root: $CONTROL_ROOT"
say "  mode: $MODE"
if [ "$MODE" = apply ] && [ "$(id -u)" -ne 0 ]; then fail "--apply must run as root (sudo ./install-class1-macos.sh --apply ...)"; fi

# Choose a free service UID in the system range (read-only).
SERVICE_UID=""
if id "$SERVICE_USER" >/dev/null 2>&1; then
  SERVICE_UID=$(id -u "$SERVICE_USER")
  say "  service identity $SERVICE_USER already exists (uid $SERVICE_UID)"
else
  used=$(dscl . -list /Users UniqueID 2>/dev/null | awk '{print $2}')
  candidate=300
  while [ "$candidate" -lt 400 ]; do
    case " $(printf '%s ' $used) " in *" $candidate "*) candidate=$((candidate + 1)) ;; *) SERVICE_UID=$candidate; break ;; esac
  done
  [ -n "$SERVICE_UID" ] || fail "no free system uid in 300-399"
  say "  service identity $SERVICE_USER will use uid $SERVICE_UID"
fi

# ---- 1. service identity -------------------------------------------------
step 1 "dedicated service identity $SERVICE_USER (no login, no home)"
if ! id "$SERVICE_USER" >/dev/null 2>&1; then
  run dscl . -create "/Groups/$SERVICE_USER"
  run dscl . -create "/Groups/$SERVICE_USER" PrimaryGroupID "$SERVICE_UID"
  run dscl . -create "/Users/$SERVICE_USER"
  run dscl . -create "/Users/$SERVICE_USER" UniqueID "$SERVICE_UID"
  run dscl . -create "/Users/$SERVICE_USER" PrimaryGroupID "$SERVICE_UID"
  run dscl . -create "/Users/$SERVICE_USER" UserShell /usr/bin/false
  run dscl . -create "/Users/$SERVICE_USER" NFSHomeDirectory /var/empty
  run dscl . -create "/Users/$SERVICE_USER" RealName "RightKit class-1 broker"
  run dscl . -create "/Users/$SERVICE_USER" IsHidden 1
else
  say "  already present; nothing to create"
fi

# ---- 2. pinned node under the control root -------------------------------
step 2 "pin node under the control root (never ~/.nvm)"
run mkdir -p "$CONTROL_ROOT/runtime/node/bin"
run install -o root -g wheel -m 0755 "$NODE_SRC" "$CONTROL_ROOT/runtime/node/bin/node"
run sh -c "shasum -a 256 '$CONTROL_ROOT/runtime/node/bin/node' > '$CONTROL_ROOT/runtime/node/NODE.sha256'"
say "  note: grant Full Disk Access / Removable Volumes to $CONTROL_ROOT/runtime/node/bin/node (FDA is per executable)"

# ---- 3. control root layout ----------------------------------------------
step 3 "control root layout (root-owned, agents read-only; runtime state owned by $SERVICE_USER)"
run mkdir -p "$CONTROL_ROOT/broker" "$CONTROL_ROOT/logs" "$CONTROL_ROOT/run"
run install -o root -g wheel -m 0644 "$BROKER_CONFIG" "$CONTROL_ROOT/broker/config.json"
# Entry points run from the stable installed runtime (package imports must resolve); they are referenced, never copied.
run chown -R root:wheel "$CONTROL_ROOT/broker" "$CONTROL_ROOT/runtime"
run chmod -R go-w "$CONTROL_ROOT/broker" "$CONTROL_ROOT/runtime"
run chown "$SERVICE_USER:$SERVICE_USER" "$CONTROL_ROOT/logs" "$CONTROL_ROOT/run"
run chmod 0750 "$CONTROL_ROOT/logs" "$CONTROL_ROOT/run"

# ---- 4. managed data roots: owned by the service identity ----------------
step 4 "managed data roots owned by $SERVICE_USER; agents keep read-only access"
for root in "$CARGO_HOME_ROOT" "$GIT_DB_ROOT" "$SCCACHE_ROOT"; do
  run chown -R "$SERVICE_USER:$SERVICE_USER" "$root"
  run chmod -R u+rwX,go-w,go+rX "$root"
done
run chown "$SERVICE_USER:$SERVICE_USER" "$BUILD_ROOT_PARENT"
run chmod 0700 "$BUILD_ROOT_PARENT"
say "  note: chown -R over a large cargo home is slow; the operator may run this step separately"

# ---- 5. launch daemons -----------------------------------------------------
write_plist() { # $1 = destination, $2 = label, $3 = user, $4 = program arguments xml, $5 = scheduling keys xml
  tmp=$(mktemp "${TMPDIR:-/tmp}/rightkit-plist.XXXXXX")
  cat > "$tmp" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$2</string>
  <key>UserName</key><string>$3</string>
  <key>GroupName</key><string>$3</string>
  <key>ProgramArguments</key>
  <array>
$4
  </array>
  <key>WorkingDirectory</key><string>$CONTROL_ROOT</string>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
  <key>StandardOutPath</key><string>$CONTROL_ROOT/logs/$2.out.log</string>
  <key>StandardErrorPath</key><string>$CONTROL_ROOT/logs/$2.err.log</string>
  <key>RunAtLoad</key><true/>
$5
</dict>
</plist>
PLIST
  if [ "$MODE" = apply ]; then run install -o root -g wheel -m 0644 "$tmp" "$1"; rm -f "$tmp"
  else say "  would write $1:"; sed 's/^/    | /' "$tmp"; rm -f "$tmp"; fi
}
step 5 "install the broker LaunchDaemon (UserName $SERVICE_USER, pinned node)"
BROKER_ARGS="    <string>$CONTROL_ROOT/runtime/node/bin/node</string>
    <string>$BROKER_ENTRY</string>
    <string>$CONTROL_ROOT/broker/config.json</string>"
BROKER_PLIST_BODY=$(printf '%s' "$BROKER_ARGS")
write_plist "$PLIST" "$LABEL" "$SERVICE_USER" "$BROKER_PLIST_BODY" "  <key>KeepAlive</key><true/>"
run plutil -lint "$PLIST"

step 6 "install the watchdog LaunchDaemon (runs as root, every 60 s)"
WATCHDOG_ARGS="    <string>$CONTROL_ROOT/runtime/node/bin/node</string>
    <string>$WATCHDOG_ENTRY</string>
    <string>--endpoint</string>
    <string>$SOCKET</string>
    <string>--lifecycle</string>
    <string>$LIFECYCLE</string>
    <string>--label</string>
    <string>$LABEL</string>"
write_plist "$WATCHDOG_PLIST" "$WATCHDOG_LABEL" "root" "$(printf '%s' "$WATCHDOG_ARGS")" "  <key>StartInterval</key><integer>60</integer>"
run plutil -lint "$WATCHDOG_PLIST"

# ---- 7. load (idempotent) --------------------------------------------------
step 7 "load both daemons (bootout first so re-runs converge)"
run sh -c "launchctl bootout system/$LABEL 2>/dev/null || true"
run launchctl bootstrap system "$PLIST"
run launchctl enable "system/$LABEL"
run sh -c "launchctl bootout system/$WATCHDOG_LABEL 2>/dev/null || true"
run launchctl bootstrap system "$WATCHDOG_PLIST"

step 8 "verify (operator)"
say "  launchctl print system/$LABEL | grep -E 'state|pid|UserName'"
say "  $CONTROL_ROOT/runtime/node/bin/node $WATCHDOG_ENTRY --endpoint $SOCKET --lifecycle $LIFECYCLE"
say "  tail -n 5 $LIFECYCLE   # expect LISTENING and HEARTBEAT events"

say ""
if [ "$MODE" = apply ]; then say "applied. rollback: scripts/uninstall-class1-macos.sh --apply"; else say "dry-run complete; nothing was changed. re-run with --apply as root to execute."; fi
