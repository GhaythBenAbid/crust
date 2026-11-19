#!/usr/bin/env bash
# Benchmark curl binaries against the local bench server.
# Usage: ./run.sh [curl-binary ...]   (defaults: system curl + Homebrew curl)
# Requires hyperfine. Start the test server separately, or set CRUST_HTTP_PORT=0
# to start one for this run. CRUST_HTTPS_PORT and CRUST_PROXY_PORT also work.
set -euo pipefail
BENCH_DIR=$(cd "$(dirname "$0")" && pwd)

BINS=("$@")
if [ ${#BINS[@]} -eq 0 ]; then
  BINS=(/usr/bin/curl)
  [ -x /opt/homebrew/opt/curl/bin/curl ] && BINS+=(/opt/homebrew/opt/curl/bin/curl)
fi

if [[ ${CRUST_HTTP_PORT:-} == 0 || ${CRUST_HTTPS_PORT:-} == 0 || ${CRUST_PROXY_PORT:-} == 0 ]]; then
  source "$BENCH_DIR/../testserver/start.sh"
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/crust-bench.XXXXXX")
  trap 'stop_testserver; rm -rf "$WORK"' EXIT
  start_testserver "$WORK"
else
  HTTP_PORT=${CRUST_HTTP_PORT:-8080}
  HTTPS_PORT=${CRUST_HTTPS_PORT:-8443}
fi
H=http://127.0.0.1:$HTTP_PORT
S=https://127.0.0.1:$HTTPS_PORT
GIB=$((1024 * 1024 * 1024))
OUT=$BENCH_DIR/results
mkdir -p "$OUT"

curl -sf "$H/small" >/dev/null || { echo "test server not responding at $H/small" >&2; exit 1; }

# name | runs | args (curl flags + URL)
SCENARIOS=(
  "startup|50|--version"
  "http_small|50|-s $H/small"
  "https_small_h1|50|-sk --http1.1 $S/small"
  "https_small_h2|50|-sk --http2 $S/small"
  "http_100_reuse|20|-s $H/small?[1-100]"
  "http_100_parallel|20|-s -Z --parallel-max 20 $H/small?[1-100]"
  "http_1gib|10|-s -o /dev/null $H/bytes?n=$GIB"
  "https_1gib_h1|10|-sk --http1.1 -o /dev/null $S/bytes?n=$GIB"
  "https_1gib_h2|10|-sk --http2 -o /dev/null $S/bytes?n=$GIB"
)

label() { "$1" --version | head -1 | awk '{print $1" "$2" ("$5")"}'; }

for sc in "${SCENARIOS[@]}"; do
  IFS='|' read -r name runs args <<<"$sc"
  cmds=() names=()
  for b in "${BINS[@]}"; do
    # shellcheck disable=SC2086
    if ! "$b" $args >/dev/null 2>&1; then
      echo "--- skip $(label "$b") for $name (unsupported)"
      continue
    fi
    cmds+=("$b $args")
    names+=(-n "$(label "$b")")
  done
  [ ${#cmds[@]} -eq 0 ] && continue
  echo "=== $name"
  hyperfine -N --warmup 3 --runs "$runs" "${names[@]}" \
    --export-json "$OUT/$name.json" "${cmds[@]}" 2>&1 | grep -vE '^\s*$'
done

# Peak memory (max RSS) and curl's own timing breakdown.
{
  echo "binary,scenario,max_rss_kib"
  for b in "${BINS[@]}"; do
    for sc in "${SCENARIOS[@]}"; do
      IFS='|' read -r name _ args <<<"$sc"
      # shellcheck disable=SC2086
      "$b" $args >/dev/null 2>&1 || continue
      rss=$(/usr/bin/time -l "$b" $args 2>&1 >/dev/null | awk '/maximum resident set size/{print int($1/1024)}')
      echo "$(label "$b"),$name,$rss"
    done
  done
} >"$OUT/memory.csv"

FMT='%{time_namelookup},%{time_connect},%{time_appconnect},%{time_starttransfer},%{time_total},%{speed_download}\n'
{
  echo "binary,scenario,namelookup,connect,tls,ttfb,total,bytes_per_sec"
  for b in "${BINS[@]}"; do
    for t in "http_small|-s -o /dev/null $H/small" \
             "https_small_h2|-sk --http2 -o /dev/null $S/small" \
             "http_1gib|-s -o /dev/null $H/bytes?n=$GIB" \
             "https_1gib_h2|-sk --http2 -o /dev/null $S/bytes?n=$GIB"; do
      IFS='|' read -r name args <<<"$t"
      # median of 5
      # shellcheck disable=SC2086
      "$b" $args >/dev/null 2>&1 || continue
      line=$(for _ in 1 2 3 4 5; do "$b" $args -w "$FMT"; done | sort -t, -k5 -n | sed -n 3p)
      echo "$(label "$b"),$name,$line"
    done
  done
} >"$OUT/timings.csv"

echo "done -> $OUT/"
