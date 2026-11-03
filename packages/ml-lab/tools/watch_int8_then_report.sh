#!/usr/bin/env bash
set -euo pipefail

EXPECTED_ROWS="${EXPECTED_ROWS:-31559}"
ROOT="${ROOT:-/workspace/a5}"
OUT_DIR="${OUT_DIR:-/workspace/a5/_overnight_20260601/rddp_trunc9L_hnm1_20260602/eval_canon31559}"
PART="${PART:-$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda.partial.jsonl}"
EVAL_LOG="${EVAL_LOG:-/workspace/a5/_overnight_20260601/rddp_trunc9L_hnm1_20260602/logs/eval_canon31559/rddp_trunc9L_hnm1_e02_rank1_int8cuda.log}"
REPORT="${REPORT:-$ROOT/scripts/hnm_bucket_recall_floor_report.py}"
WATCH_LOG="${WATCH_LOG:-$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda.report_watch.log}"
OUT_TSV="${OUT_TSV:-$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda_bucket_report.tsv}"
OUT_MD="${OUT_MD:-$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda_bucket_report.md}"
COMPLETE_JSONL="${COMPLETE_JSONL:-$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda.complete.jsonl}"

log() {
  printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$WATCH_LOG"
}

log "watcher start expected_rows=$EXPECTED_ROWS part=$PART"

while true; do
  rows=0
  [[ -f "$PART" ]] && rows=$(wc -l < "$PART")
  proc="$(pgrep -af 'eval_ssl_ood_chunked.py.*int8_cuda' | grep -v grep || true)"
  oom_count=0
  [[ -f "$EVAL_LOG" ]] && oom_count=$(grep -ci 'out of memory\|cuda out' "$EVAL_LOG" || true)

  if [[ "$oom_count" != "0" ]]; then
    log "ERROR oom_count=$oom_count rows=$rows"
    exit 2
  fi

  if (( rows >= EXPECTED_ROWS )); then
    if [[ -n "$proc" ]]; then
      log "rows=$rows complete-sized but eval still alive; waiting for clean exit"
      sleep 60
      continue
    fi

    log "complete rows=$rows; generating corrected bucket report"
    cp -f "$PART" "$COMPLETE_JSONL"
    cd "$ROOT"
    export PYTHONPATH="$ROOT/app:$ROOT"
    /venv/main/bin/python "$REPORT" \
      --jsonl "$COMPLETE_JSONL" \
      --family "rddp_trunc9L_hnm1" \
      --label "rddp_trunc9L_hnm1_e02_rank1_int8cuda" \
      --out-tsv "$OUT_TSV" \
      --out-md "$OUT_MD" \
      --expected-rows "$EXPECTED_ROWS" | tee -a "$WATCH_LOG"
    log "report done tsv=$OUT_TSV md=$OUT_MD jsonl=$COMPLETE_JSONL"
    touch "$OUT_DIR/rddp_trunc9L_hnm1_e02_rank1_int8cuda.report_done"
    exit 0
  fi

  if [[ -z "$proc" ]]; then
    log "ERROR eval exited before complete rows=$rows expected=$EXPECTED_ROWS"
    exit 3
  fi

  log "waiting rows=$rows/$EXPECTED_ROWS"
  sleep 60
done
