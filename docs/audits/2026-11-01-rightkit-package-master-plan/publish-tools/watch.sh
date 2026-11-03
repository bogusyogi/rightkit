#!/bin/bash
S=/tmp/claude-0/-home-user/52c3079d-4ab0-5142-8c1b-b29be4523c74/scratchpad
touch $S/seen.txt
check() { key="$1"; val="$2"; [ -z "$val" ] && return; if ! grep -qxF "$key $val" $S/seen.txt; then if grep -q "^$key " $S/seen.txt; then echo "PUBLISHED $key $val"; fi; echo "$key $val" >> $S/seen.txt; fi; }
while true; do
  for c in credentials tauri-shell compositor shorts media-fetch remote-gateway dictation-text-pipeline agent-orchestration agent-tools agents content-plan memory-core motion qa repo-graph social-inbox; do
    p=$(node -e 'const n=process.argv[1];console.log(n.slice(0,2)+"/"+n.slice(2,4)+"/"+n)' rightkit-$c)
    v=$(curl -s -A x https://index.crates.io/$p | tail -1 | grep -o '"vers":"[^"]*"' | cut -d'"' -f4)
    check "rightkit-$c" "$v"
  done
  for p in agent-ui-hooks html-editor markdown-editor terminal-ui qa; do
    v=$(curl -s https://registry.npmjs.org/@rightkit%2F$p | grep -o '"latest":"[^"]*"' | cut -d'"' -f4)
    check "@rightkit/$p" "$v"
  done
  sleep 120
done
