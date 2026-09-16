# shellcheck shell=bash
# Readiness polling against a deadline.
#
# The HTTP and gRPC listeners are spawned as independent tasks after the
# server finishes building, so a bound HTTP port does not imply the gRPC port
# is bound yet. Both are polled.

qual_wait_http() {
  local port="$1" deadline_seconds="$2"
  local deadline=$(( $(date +%s) + deadline_seconds ))
  while (( $(date +%s) < deadline )); do
    if curl --silent --show-error --fail --max-time 3 "http://127.0.0.1:${port}/status" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "error: HTTP port ${port} did not become ready within ${deadline_seconds}s" >&2
  return 1
}

qual_wait_tcp() {
  local port="$1" deadline_seconds="$2"
  local deadline=$(( $(date +%s) + deadline_seconds ))
  while (( $(date +%s) < deadline )); do
    if python3 -c "
import socket, sys
s = socket.socket()
s.settimeout(2)
try:
    s.connect(('127.0.0.1', ${port}))
except OSError:
    sys.exit(1)
finally:
    s.close()
" 2>/dev/null; then
      return 0
    fi
    sleep 1
  done
  echo "error: port ${port} did not accept connections within ${deadline_seconds}s" >&2
  return 1
}

qual_wait_ready() {
  local http_port="$1" grpc_port="$2" deadline_seconds="${3:-180}"
  qual_wait_http "${http_port}" "${deadline_seconds}" || return 1
  qual_wait_tcp "${grpc_port}" "${deadline_seconds}" || return 1
}
