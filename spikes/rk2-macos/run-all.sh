#!/bin/zsh
cd "$(dirname "$0")"
python3 run.py offscreen 30 60
python3 run.py offscreen 60 60
RK2_PRE=none python3 run.py offscreen 30 20
python3 run.py occluded 30 60
python3 run.py apphidden 30 60
python3 run.py minimized 30 60
python3 run.py offscreen 30 15 file
for d in evidence/*/; do [ -f "$d/summary.json" ] && python3 analyze.py "$d" > "$d/analyze.out" 2>&1; done
echo ALLDONE
