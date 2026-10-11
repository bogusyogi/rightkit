#!/bin/bash
# usage: cmp.sh dirA dirB  -> prints per-file sha match
cd "$1" || exit 1
for f in *.png; do
  a=$(shasum -a 256 "$1/$f" | cut -d' ' -f1)
  b=$(shasum -a 256 "$2/$f" 2>/dev/null | cut -d' ' -f1)
  if [ "$a" = "$b" ]; then echo "SAME $f"; else echo "DIFF $f"; fi
done
