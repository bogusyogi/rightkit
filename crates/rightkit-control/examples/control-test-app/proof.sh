#!/bin/sh
# Manual end-to-end proof of the in-app control server over rightkit-daemon (macOS).
# Build first (from rightkit):
#   rightkit cargo test -p rightkit-control --features qa-control --lib
#   rightkit cargo build --manifest-path crates/rightkit-control/examples/control-test-app/Cargo.toml
#   rightkit cargo build -p rightkit-control -p rightkit-daemon -p rightkit-qa --bins --example svc_call
# Then: TARGET=<workspace managed target>/debug APP_TARGET=<fixture managed target>/debug sh proof.sh
# Launches the app hidden (open -g -j -n), calls it as the allowlisted app `right-qa`
# through the service socket, checks the frontmost app never changed, kills it.
set -eu
BIN=${TARGET:?set TARGET to the cargo target debug dir}
APP_BIN=${APP_TARGET:-$BIN}
T=$(mktemp -d /tmp/rkct.XXXX); APP=$T/ct.app
PID=; MONITOR=
cleanup() {
  touch "$T/stop-monitor"
  if [ -n "$MONITOR" ]; then wait "$MONITOR" || true; fi
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; fi
  rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 1' INT TERM
mkdir -p "$APP/Contents/MacOS"; cp "$APP_BIN/control-test-app" "$APP/Contents/MacOS/control-test-app"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>control-test-app</string><key>CFBundleIdentifier</key><string>app.rightkit.ct</string><key>CFBundleName</key><string>ct</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>
PL
CALL="$BIN/examples/svc_call"; FRONT0=$("$BIN/rightkit-control" frontmost)
case "$FRONT0" in '0 '*|'') echo FRONTMOST_UNMEASURABLE; exit 1;; esac
(
  while [ ! -f "$T/stop-monitor" ]; do
    [ "$FRONT0" = "$("$BIN/rightkit-control" frontmost)" ] || touch "$T/frontmost-changed"
    sleep 0.05
  done
) & MONITOR=$!
open -g -j -n --env RIGHTKIT_QA_HIDDEN=1 --env RIGHTKIT_QA_BACKGROUND=1 --env RIGHTKIT_CONTROL_SERVICE=control-ct --env RIGHTKIT_SUITE_ROOT="$T/suite" "$APP"
i=0; while [ ! -f "$T/suite/runtime/control-ct/service.json" ] && [ $i -lt 40 ]; do sleep 0.5; i=$((i+1)); done
cat "$T/suite/runtime/control-ct/service.json"; echo
PID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pid"])' "$T/suite/runtime/control-ct/service.json")
call() { "$CALL" "$T/suite" control-ct right-qa "$@"; }
assert_eval() {
  body=$(python3 -c 'import json,sys; print(json.dumps({"js":sys.argv[1]}))' "$1")
  i=0
  until result=$(call eval "$body") && printf '%s' "$result" | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("value") is True else 1)'; do
    i=$((i+1)); [ "$i" -lt 20 ] || { echo "ASSERT_FAILED: $2: $result"; exit 1; }
    sleep 0.05
  done
  echo "$2"
}
assert_hidden_window() {
  state=$(call command '{"name":"window_state"}')
  printf '%s' "$state" | python3 -c 'import json,sys; s=json.load(sys.stdin)["result"]; assert (s["key"],s["visible"]) == (False,False),s'
}
i=0; until "$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.readyState;"}' 2>/dev/null | grep -q complete || [ $i -ge 20 ]; do sleep 0.5; i=$((i+1)); done
echo "health:  $("$CALL" "$T/suite" control-ct right-qa health)"
echo "eval:    $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.title;"}')"
echo "command: $("$CALL" "$T/suite" control-ct right-qa command '{"name":"ping","args":""}')"
echo "shot:    $("$CALL" "$T/suite" control-ct right-qa screenshot)"
assert_hidden_window
assert_eval 'return !document.hasFocus() && document.visibilityState === "hidden";' HIDDEN_NONFOCUSED_PAGE
# Repeat from a non-key state to exclude the intermittent key-window success case.
for run in 1 2 3; do
  call eval '{"js":"window.__state.scrubEvents=[];return true;"}' >/dev/null
  coords=$(call eval '{"js":"const r=document.getElementById(\"scrub\").getBoundingClientRect();return [r.x+20,r.y+r.height/2,r.x+340];"}')
  read -r x1 y x2 <<COORDS
$(printf '%s' "$coords" | python3 -c 'import json,sys; print(*json.load(sys.stdin)["value"])')
COORDS
  call pointer "{\"phase\":\"down\",\"x\":$x1,\"y\":$y}" >/dev/null
  assert_hidden_window
  assert_eval 'return window.__state.scrubActive && window.__state.scrubEvents.some(e=>e.type==="pointerdown" && e.trusted && e.buttons===1);' POINTER_PRESS_OK
  call pointer "{\"phase\":\"drag\",\"x\":$x2,\"y\":$y}" >/dev/null
  assert_hidden_window
  assert_eval 'return window.__state.scrubValue>=80 && window.__state.scrubEvents.some(e=>e.type==="pointermove" && e.buttons===1 && e.captured);' POINTER_DRAG_OK
  call pointer "{\"phase\":\"up\",\"x\":$x2,\"y\":$y}" >/dev/null
  assert_hidden_window
  assert_eval 'const s=window.__state,es=s.scrubEvents,d=es.findIndex(e=>e.type==="pointerdown"),u=es.findIndex(e=>e.type==="pointerup");return !s.scrubActive && s.scrubValue>=80 && d>=0 && u>d && es[u].buttons===0 && es.every(e=>e.trusted && !e.focus && e.visibility==="hidden" && e.pointerId===es[d].pointerId);' HIDDEN_SCRUB_OK
done
# The composite API must use the same native press/drag/release path.
call drag "{\"x1\":$x1,\"y1\":$y,\"x2\":$x2,\"y2\":$y,\"steps\":8,\"modifiers\":[\"shift\"]}" >/dev/null
assert_eval 'const s=window.__state;return !s.scrubActive && s.scrubValue>=80 && s.scrubEvents.some(e=>e.type==="pointerdown" && e.modifiers[0]);' COMPOSITE_SCRUB_OK
coords=$(call eval '{"js":"const r=document.getElementById(\"btn\").getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2];"}')
read -r bx by <<COORDS
$(printf '%s' "$coords" | python3 -c 'import json,sys; print(*json.load(sys.stdin)["value"])')
COORDS
call click "{\"x\":$bx,\"y\":$by}" >/dev/null
for modifier in shift cmd alt ctrl; do
  call click "{\"x\":$bx,\"y\":$by,\"modifiers\":[\"$modifier\"]}" >/dev/null
  assert_hidden_window
done
assert_eval 'const s=window.__state;return s.clicks>=4 && s.trusted.every(Boolean) && [[true,false,false,false],[false,true,false,false],[false,false,true,false]].every(m=>s.clickModifiers.some(v=>JSON.stringify(v)===JSON.stringify(m))) && s.buttonPointers.some(e=>e.type==="pointerdown" && e.trusted && e.modifiers[3]);' TRUSTED_MODIFIER_CLICKS_OK
"$CALL" "$T/suite" control-ct right-qa eval '{"js":"document.getElementById(\"inp\").focus();return 1;"}' >/dev/null
echo "type:    $("$CALL" "$T/suite" control-ct right-qa type '{"text":"say \"hi\" \\ caf\u00e9"}')"
echo "typed-value: $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.getElementById(\"inp\").value;"}')"
# Editable focus exercises WebKit's text-input interception path as well.
assert_eval 'return document.activeElement === document.getElementById("inp");' EDITABLE_FOCUS_READY
call eval '{"js":"window.__state.scrubEvents=[];return true;"}' >/dev/null
call drag "{\"x1\":$x1,\"y1\":$y,\"x2\":$x2,\"y2\":$y,\"steps\":8}" >/dev/null
assert_eval 'const s=window.__state;return !s.scrubActive && s.scrubValue>=80 && s.scrubEvents.some(e=>e.type==="pointerdown" && e.buttons===1) && s.scrubEvents.some(e=>e.type==="pointerup" && e.buttons===0) && s.scrubEvents.every(e=>e.trusted);' EDITABLE_TO_SCRUB_OK
assert_hidden_window
echo "shot-masked: $("$CALL" "$T/suite" control-ct right-qa screenshot | grep -o '"masked":[0-9]*')"
echo "restored:    $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return getComputedStyle(document.getElementById(\"pw\")).visibility;"}')"
"$CALL" "$T/suite" control-ct right-qa eval '{"js":"window.__state.keys.length=0;document.body.focus();return 1;"}' >/dev/null
for k in up down left right home end pageup pagedown forwarddelete f1 f12 comma cmd+,; do "$CALL" "$T/suite" control-ct right-qa key "{\"key\":\"$k\"}" >/dev/null; done
KEYS=$("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return window.__state.keys.join(\" \");"}')
echo "named keys: $KEYS"
echo "$KEYS" | grep -q "ArrowUp ArrowDown ArrowLeft ArrowRight Home End PageUp PageDown Delete F1 F12 , ,+cmd" && echo NAMED_KEYS_OK || { echo NAMED_KEYS_WRONG; exit 1; }
echo "unlisted app:$("$CALL" "$T/suite" control-ct someone-else health || true)"
echo "unlisted method: $("$CALL" "$T/suite" control-ct right-qa nope || true)"
[ "$FRONT0" = "$("$BIN/rightkit-control" frontmost)" ] && [ ! -f "$T/frontmost-changed" ] && echo FRONTMOST_UNCHANGED || { echo FRONTMOST_CHANGED; exit 1; }
kill "$PID"; PID=; sleep 1
"$CALL" "$T/suite" control-ct right-qa health || true
