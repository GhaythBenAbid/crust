# Source from Bash to build and start an isolated test server.
start_testserver() {
  local work=$1 attempt extra
  (cd "$(dirname "${BASH_SOURCE[0]}")" && go build -o "$work/testserver" .) || return 1
  : >"$work/server.ports" || return 1
  CRUST_HTTP_PORT=${CRUST_HTTP_PORT:-0} CRUST_HTTPS_PORT=${CRUST_HTTPS_PORT:-0} \
    CRUST_PROXY_PORT=${CRUST_PROXY_PORT:-0} CERT_OUT="$work/cert.pem" \
    PROXY_AUTH="" "$work/testserver" >"$work/server.ports" 2>"$work/server.log" &
  SERVER_PID=$!

  for ((attempt = 0; attempt < 50; attempt++)); do
    if read -r HTTP_PORT HTTPS_PORT PROXY_PORT extra <"$work/server.ports" &&
       [[ $HTTP_PORT =~ ^[0-9]+$ && $HTTPS_PORT =~ ^[0-9]+$ && $PROXY_PORT =~ ^[0-9]+$ && -z $extra ]]; then
      return 0
    fi
    kill -0 "$SERVER_PID" 2>/dev/null || break
    sleep 0.1
  done
  echo "Test server failed to report its bound ports" >&2
  cat "$work/server.log" >&2
  return 1
}

stop_testserver() {
  if [[ -n ${SERVER_PID:-} ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
