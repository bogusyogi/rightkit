#!/bin/sh
# Manual end-to-end proof of the in-app control server over rightkit-service (macOS).
# Build first (from tools/rightkit):
#   rightkit cargo build --manifest-path crates/rightkit-control/examples/control-test-app/Cargo.toml
#   rightkit cargo build -p rightkit-service --examples -p rightkit-control
# Then: TARGET=<managed target dir>/debug sh proof.sh
# Launches the app hidden (open -g -j -n), calls it as the allowlisted app `right-qa`
# through the service socket, checks the frontmost app never changed, kills it.
set -eu
BIN=${TARGET:?set TARGET to the cargo target debug dir}
T=$(mktemp -d /tmp/rkct.XXXX); APP=$T/ct.app
mkdir -p "$APP/Contents/MacOS"; cp "$BIN/control-test-app" "$APP/Contents/MacOS/control-test-app"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>control-test-app</string><key>CFBundleIdentifier</key><string>app.rightkit.ct</string><key>CFBundleName</key><string>ct</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>
PL
CALL="$BIN/examples/svc_call"; FRONT0=$("$BIN/rightkit-control" frontmost)
open -g -j -n --env RIGHTKIT_CONTROL_SERVICE=control-ct --env RIGHTKIT_SUITE_ROOT="$T/suite" "$APP"
i=0; while [ ! -f "$T/suite/runtime/control-ct/service.json" ] && [ $i -lt 40 ]; do sleep 0.5; i=$((i+1)); done
cat "$T/suite/runtime/control-ct/service.json"; echo
i=0; until "$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.readyState;"}' 2>/dev/null | grep -q complete || [ $i -ge 20 ]; do sleep 0.5; i=$((i+1)); done
echo "health:  $("$CALL" "$T/suite" control-ct right-qa health)"
echo "eval:    $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.title;"}')"
echo "command: $("$CALL" "$T/suite" control-ct right-qa command '{"name":"ping","args":""}')"
echo "shot:    $("$CALL" "$T/suite" control-ct right-qa screenshot)"
"$CALL" "$T/suite" control-ct right-qa eval '{"js":"document.getElementById(\"inp\").focus();return 1;"}' >/dev/null
echo "type:    $("$CALL" "$T/suite" control-ct right-qa type '{"text":"say \"hi\" \\ caf\u00e9"}')"
echo "typed-value: $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return document.getElementById(\"inp\").value;"}')"
echo "shot-masked: $("$CALL" "$T/suite" control-ct right-qa screenshot | grep -o '"masked":[0-9]*')"
echo "restored:    $("$CALL" "$T/suite" control-ct right-qa eval '{"js":"return getComputedStyle(document.getElementById(\"pw\")).visibility;"}')"
echo "unlisted app:    $("$CALL" "$T/suite" control-ct someone-else health || true)"
echo "unlisted method: $("$CALL" "$T/suite" control-ct right-qa nope || true)"
[ "$FRONT0" = "$("$BIN/rightkit-control" frontmost)" ] && echo FRONTMOST_UNCHANGED || { echo FRONTMOST_CHANGED; exit 1; }
pkill -f "$APP/Contents/MacOS/control-test-app" || true; sleep 1
"$CALL" "$T/suite" control-ct right-qa health || true
rm -rf "$T"
