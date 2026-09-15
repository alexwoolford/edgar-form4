#!/usr/bin/env bash
# Oneshot: ingest yesterday's (or EDGAR_INGEST_DATE) Form 4 code-P purchases into work sqlite.
# No published current/ copy.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${EDGAR_FORM4_BIN:-$ROOT/bin/edgar-form4}"
STATE="${EDGAR_FORM4_STATE:-/var/lib/edgar-form4}"
DB="${EDGAR_FORM4_SQLITE:-$STATE/edgar-form4.sqlite}"
LOCK="${EDGAR_FORM4_LOCK:-$STATE/.ingest.lock}"

test -x "$BIN" || {
  echo "missing $BIN — build with: cargo build --release" >&2
  exit 1
}

mkdir -p "$STATE"

acquire_lock() {
  if command -v flock >/dev/null 2>&1; then
    exec 9>"$LOCK"
    if ! flock -n 9; then
      echo "ingest already running (lock $LOCK)" >&2
      exit 1
    fi
  else
    if ! mkdir "$LOCK.d" 2>/dev/null; then
      echo "ingest already running (lock $LOCK.d)" >&2
      exit 1
    fi
    trap 'rmdir "$LOCK.d" 2>/dev/null || true' EXIT
  fi
}
acquire_lock

DATE_ARGS=()
if [[ -n "${EDGAR_INGEST_FROM:-}" || -n "${EDGAR_INGEST_TO:-}" ]]; then
  if [[ -n "${EDGAR_INGEST_DATE:-}" ]]; then
    echo "EDGAR_INGEST_DATE cannot be combined with EDGAR_INGEST_FROM/TO" >&2
    exit 1
  fi
  if [[ -z "${EDGAR_INGEST_FROM:-}" || -z "${EDGAR_INGEST_TO:-}" ]]; then
    echo "EDGAR_INGEST_FROM and EDGAR_INGEST_TO must both be set" >&2
    exit 1
  fi
  DATE_ARGS=(--from "$EDGAR_INGEST_FROM" --to "$EDGAR_INGEST_TO")
elif [[ -n "${EDGAR_INGEST_DATE:-}" ]]; then
  DATE_ARGS=(--date "$EDGAR_INGEST_DATE")
fi

echo "== edgar-form4 ingest =="
echo "bin=$BIN db=$DB"
"$BIN" --db "$DB" ingest "${DATE_ARGS[@]}"
