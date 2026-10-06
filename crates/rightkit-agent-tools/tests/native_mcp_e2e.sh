#!/bin/sh
# Real native fixture proof: hidden control-test-app -> rightkit-service -> right-ctl CLI, then optional MCP.
# Build first through RightKit, then run with TARGET=<managed target>/debug:
#   rightkit cargo build -p rightkit-agent-tools --features mcp
#   rightkit cargo build --manifest-path crates/rightkit-control/examples/control-test-app/Cargo.toml
#   rightkit cargo build -p rightkit-service --examples -p rightkit-control
#   TARGET=... sh crates/rightkit-agent-tools/tests/native_mcp_e2e.sh
set -eu

BIN=${TARGET:?set TARGET to the managed cargo target debug dir}
CTL=${RIGHT_CTL_BIN:-$BIN/right-ctl}
CONTROL=${CONTROL_APP_BIN:-$BIN/control-test-app}
SVC_CALL=${SVC_CALL_BIN:-$BIN/examples/svc_call}
RK_CONTROL=${RK_CONTROL_BIN:-$BIN/rightkit-control}
CONTROL_CONFIG=$(CDPATH= cd -- "$(dirname "$0")/../../rightkit-control/examples/control-test-app" && pwd)/tauri.conf.json
CONTROL_SOURCE=$(CDPATH= cd -- "$(dirname "$0")/../../rightkit-control/examples/control-test-app" && pwd)/src/main.rs
grep -Eq '"visible"[[:space:]]*:[[:space:]]*false' "$CONTROL_CONFIG"
if grep -Eq '\.show[[:space:](]' "$CONTROL_SOURCE"; then
  echo "control-test-app contains a downstream show call" >&2
  exit 1
fi
T=$(mktemp -d /tmp/rk-agent-mcp.XXXX)
APP=$T/control-test.app
SUITE=$T/suite
mkdir -p "$APP/Contents/MacOS"
cp "$CONTROL" "$APP/Contents/MacOS/control-test-app"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>control-test-app</string><key>CFBundleIdentifier</key><string>app.rightkit.ct</string><key>CFBundleName</key><string>ct</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>
PLIST

cleanup() {
  if [ -n "${MON:-}" ]; then kill "$MON" 2>/dev/null || true; wait "$MON" 2>/dev/null || true; fi
  pkill -f "$APP/Contents/MacOS/control-test-app" 2>/dev/null || true
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

FRONT0=$($RK_CONTROL frontmost)
FRONT_PID=$(printf '%s\n' "$FRONT0" | awk '{print $1}')
WINDOW_LOG=$T/windows.log
FRONT_LOG=$T/frontmost.log
PID_FILE=$T/pid
check_hidden() {
  pid=$(cat "$PID_FILE")
  "$RK_CONTROL" windows "$pid" >> "$WINDOW_LOG"
  if grep -Eq 'onscreen=(true|1)' "$WINDOW_LOG"; then
    echo "OSWindowServer reported onscreen window" >&2
    return 1
  fi
}
monitor() {
  end=$(( $(date +%s) + 50 ))
  while [ "$(date +%s)" -lt "$end" ]; do
    front=$($RK_CONTROL frontmost) || { echo MONITOR_FAILURE >> "$FRONT_LOG"; exit 1; }
    printf '%s\n' "$front" >> "$FRONT_LOG"
    if [ -s "$PID_FILE" ]; then
      fixture=$(cat "$PID_FILE")
      if printf '%s\n' "$front" | awk -v bad="$fixture" 'NF && $1 == bad { found=1 } END { exit found ? 0 : 1 }'; then
        echo "fixture became frontmost" >&2
        echo MONITOR_FAILURE >> "$FRONT_LOG"
        exit 1
      fi
      check_hidden || { echo MONITOR_FAILURE >> "$FRONT_LOG"; exit 1; }
    fi
    sleep 0.2
  done
}
monitor & MON=$!
open -g -j -n --env RIGHTKIT_CONTROL_SERVICE=control-ct --env RIGHTKIT_SUITE_ROOT="$SUITE" "$APP"
i=0
while [ ! -f "$SUITE/runtime/control-ct/service.json" ] && [ "$i" -lt 40 ]; do
  sleep 0.5
  i=$((i + 1))
done
[ -f "$SUITE/runtime/control-ct/service.json" ]
PID=$(python3 - "$SUITE/runtime/control-ct/service.json" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1]))["pid"])
PY
)
printf '%s\n' "$PID" > "$PID_FILE"
check_hidden

# Preferred surface: direct right-ctl CLI over the same rightkit-service socket.
CLI_HEALTH=$("$CTL" --suite-root "$SUITE" ct health)
CLI_ECHO=$("$CTL" --suite-root "$SUITE" ct command echo hello)
CLI_PING=$("$CTL" --suite-root "$SUITE" ct command ping)
python3 - "$CLI_HEALTH" "$CLI_ECHO" "$CLI_PING" <<'PY'
import json
import sys
health, echo, ping = map(json.loads, sys.argv[1:])
print("CLI_PAYLOADS", json.dumps(health, sort_keys=True), json.dumps(echo, sort_keys=True), json.dumps(ping, sort_keys=True))
assert health["ok"] is True
assert echo["ok"] is True and echo["result"]["echo"] == "hello", echo
assert ping["ok"] is True and ping["result"] == "pong", ping
print("CLI_HEALTH_ECHO_PING_PASS")
PY
if "$CTL" --suite-root "$SUITE" ct command paste > "$T/cli-exposure.log" 2>&1; then
  echo "direct CLI exposure denial unexpectedly allowed" >&2
  exit 1
fi
grep -q '"code":"not_exposed"' "$T/cli-exposure.log"
echo "CLI_EXPOSURE_DENIAL_PASS"

python3 - "$CTL" "$SUITE" "$SVC_CALL" "$FRONT0" "$RK_CONTROL" <<'PY'
import atexit
import json
import signal
import subprocess
import sys

ctl, suite, svc_call, front0, rk_control = sys.argv[1:]
def alarm(_sig, _frame):
    raise TimeoutError("native MCP proof timed out")
signal.signal(signal.SIGALRM, alarm)
signal.alarm(30)
p = subprocess.Popen([ctl, "--suite-root", suite, "mcp-serve", "ct"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
def close_child():
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
atexit.register(close_child)

def call(i, method, params):
    p.stdin.write(json.dumps({"jsonrpc":"2.0", "id":i, "method":method, "params":params}) + "\n")
    p.stdin.flush()
    return json.loads(p.stdout.readline())

init = call(1, "initialize", {"protocolVersion":"2025-03-26"})
assert init["result"]["serverInfo"]["name"] == "right-ctl"
p.stdin.write(json.dumps({"jsonrpc":"2.0", "method":"notifications/initialized"}) + "\n")
p.stdin.flush()
listed = call(2, "tools/list", {})
names = {x["name"] for x in listed["result"]["tools"]}
assert {"ct.health", "ct.command"}.issubset(names)
health = call(3, "tools/call", {"name":"ct.health", "arguments":{}})
assert health["result"]["structuredContent"]["ok"] is True
command = call(4, "tools/call", {"name":"ct.command", "arguments":{"name":"echo", "args":"hello"}})
assert command["result"]["structuredContent"]["result"]["echo"] == "hello"
blocked = call(5, "tools/call", {"name":"ct.command", "arguments":{"name":"paste", "args":""}})
assert blocked["result"]["isError"] is True
assert blocked["result"]["structuredContent"]["code"] == "not_exposed"
p.stdin.close()
p.wait(timeout=10)
assert p.returncode == 0, p.stderr.read()

def svc(app, method):
    return subprocess.run([svc_call, suite, "control-ct", app, method], text=True, capture_output=True)
assert svc("someone-else", "health").returncode != 0
assert svc("right-qa", "nope").returncode != 0
print("OPTIONAL_MCP_INTEROP_PASS")
print("MCP initialize/list/health/command: PASS")
print("MCP hard exposure paste denial: PASS")
print("MCP service allowlist app/method denials: PASS")
PY
kill "$MON" 2>/dev/null || true
wait "$MON" 2>/dev/null || true
MON=
awk 'NF { count++ } END { exit count < 1 }' "$FRONT_LOG"
if awk -v fixture="$PID" 'NF && $1 == fixture { found=1 } END { exit found ? 0 : 1 }' "$FRONT_LOG"; then
  echo "fixture became frontmost" >&2
  exit 1
fi
if grep -q '^MONITOR_FAILURE$' "$FRONT_LOG"; then
  echo "continuous frontmost/window monitor failed" >&2
  exit 1
fi
if grep -Eq 'onscreen=(true|1)' "$WINDOW_LOG"; then
  echo "OSWindowServer reported onscreen window" >&2
  exit 1
fi
echo "QA_FIXTURE_NEVER_FOREGROUND (continuous)"
echo "OSWINDOWSERVER_NO_ONSCREEN_WINDOWS"
