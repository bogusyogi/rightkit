#!/bin/bash
# shellcheck disable=SC2086  # archived evidence: the command exactly as it was run on 2026-10-11, kept unchanged
# usage: step.sh <name> <prevDir>  -> captures to ui-captures/<name>, compares to prevDir, writes result file
S=/private/tmp/claude-501/-Volumes-D-claude-genright/fa1450cb-0be5-40b4-8c64-7c625a35aa51/scratchpad
NAME=$1; PREV=$2; OUT=$S/ui-captures/$NAME
rm -f $S/ui-captures/$NAME.done
node $S/tools/capture.mjs "http://127.0.0.1:5199/?qa=1&qaState=app" "$OUT" > $S/ui-captures/$NAME.log 2>&1
bash $S/tools/cmp.sh "$PREV" "$OUT" > $S/ui-captures/$NAME.cmp.txt
touch $S/ui-captures/$NAME.done
