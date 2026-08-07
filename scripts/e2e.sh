#!/usr/bin/env bash
# End-to-end protocol tests for the MCP server component.
#
# Builds the component, serves it with `wasmtime serve -Sp3,cli,http`, and
# exercises the MCP 2026-07-28 streamable-HTTP protocol plus the failure
# modes an adversarial client can trigger. Requires: cargo (wasm32-wasip2
# target), wasmtime >= 46, curl, python3.
#
# Usage: scripts/e2e.sh [--no-build]
set -u

cd "$(dirname "$0")/.."

PORT=8199
GUARD_PORT=8198
FIXTURE_PORT=8197
BASE="http://127.0.0.1:${PORT}/"
WASM=target/wasm32-wasip2/release/mcp_server_template.wasm
META='"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}'
ACCEPT='Accept: application/json, text/event-stream'
CT='Content-Type: application/json'
PV='MCP-Protocol-Version: 2026-07-28'

PASS=0
FAIL=0
FAILED_NAMES=()

pass() { PASS=$((PASS + 1)); echo "  ok   - $1"; }
fail() {
  FAIL=$((FAIL + 1))
  FAILED_NAMES+=("$1")
  echo "  FAIL - $1"
  echo "         $2" | head -c 500
  echo
}

# assert_contains <name> <needle> <haystack>
assert_contains() {
  case "$3" in
    *"$2"*) pass "$1" ;;
    *) fail "$1" "expected to contain [$2], got: $3" ;;
  esac
}

# assert_not_contains <name> <needle> <haystack>
assert_not_contains() {
  case "$3" in
    *"$2"*) fail "$1" "expected NOT to contain [$2], got: $3" ;;
    *) pass "$1" ;;
  esac
}

# mcp_post <extra curl args...> — POST a body given on stdin with standard headers
mcp_post() {
  curl -sS --max-time 20 -X POST "$BASE" -H "$CT" -H "$ACCEPT" -H "$PV" "$@" --data-binary @-
}

cleanup() {
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "${GUARD_PID:-}" ] && kill "$GUARD_PID" 2>/dev/null
  [ -n "${FIXTURE_PID:-}" ] && kill "$FIXTURE_PID" 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

if [ "${1:-}" != "--no-build" ]; then
  echo "building component..."
  cargo build --release || exit 1
fi
[ -f "$WASM" ] || { echo "missing $WASM"; exit 1; }

echo "verifying component world..."
if wasm-tools component wit "$WASM" 2>/dev/null | grep -q 'export wasi:http/handler@0.3.0'; then
  pass "component exports wasi:http/handler@0.3.0"
else
  fail "component exports wasi:http/handler@0.3.0" "export missing from component world"
fi

# Local HTTP fixture server for outbound tests: serves a >4 KiB multibyte
# body, a hello file, and a /hang endpoint that never responds (black-holed
# upstream, exercised by the outbound-timeout test).
python3 - "$FIXTURE_PORT" <<'EOF' >/dev/null 2>&1 &
import sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/hang":
            time.sleep(600)  # accept, never respond
            return
        if self.path == "/multibyte.txt":
            # 6000 bytes of 3-byte UTF-8 chars: byte-offset truncation lands mid-char.
            body = ("☃" * 2000).encode()
        else:
            body = b"hello from fixture"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
EOF
FIXTURE_PID=$!

echo "starting wasmtime serve on :${PORT}..."
# MCP_HTTP_GET_ALLOW_LOCAL lets http_get reach the local fixture server; the
# guard instance below runs WITHOUT it to test the SSRF denial default. The
# short outbound timeout keeps the black-hole-upstream test fast.
wasmtime serve -Sp3,cli,http --env MCP_HTTP_GET_ALLOW_LOCAL=true \
  --env MCP_OUTBOUND_TIMEOUT_MS=3000 \
  --addr "127.0.0.1:${PORT}" "$WASM" >/tmp/mcp-e2e-server.log 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  curl -s -o /dev/null "http://127.0.0.1:${PORT}/" && break
  sleep 0.2
done

echo "== protocol =="

OUT=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' | mcp_post)
assert_contains "initialize negotiates 2026-07-28" '"protocolVersion":"2026-07-28"' "$OUT"

HDRS=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' | mcp_post -i -o /dev/null -D - 2>/dev/null || true)
HDRS=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' | curl -sS --max-time 20 -X POST "$BASE" -H "$CT" -H "$ACCEPT" -H "$PV" -D - -o /dev/null --data-binary @-)
assert_contains "initialize streams as SSE" 'text/event-stream' "$HDRS"

OUT=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"old","version":"0"}}}' | mcp_post)
assert_contains "older protocol client still served (statelessly)" '"jsonrpc":"2.0"' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{$META}}" | mcp_post -H 'Mcp-Method: tools/list')
for TOOL in add current_time echo http_get; do
  assert_contains "tools/list contains $TOOL" "\"name\":\"$TOOL\"" "$OUT"
done

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"echo\",\"arguments\":{\"message\":\"e2e says hi\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: echo')
assert_contains "echo round-trips" 'e2e says hi' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"add\",\"arguments\":{\"a\":20.5,\"b\":21.5},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: add')
assert_contains "add returns structuredContent" '"structuredContent":{"sum":42.0}' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"current_time\",$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: current_time')
MS=$(printf '%s' "$OUT" | grep -o '"text":"[0-9]*"' | grep -o '[0-9]*' | head -1)
if [ -n "$MS" ] && [ "$MS" -gt 1000000000000 ] 2>/dev/null; then
  pass "current_time returns plausible epoch millis"
else
  fail "current_time returns plausible epoch millis" "$OUT"
fi

echo "== spec enforcement =="

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/list\",\"params\":{$META}}" | mcp_post)
assert_contains "missing Mcp-Method header rejected" 'Mcp-Method' "$OUT"

OUT=$(printf '%s' '{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}' | mcp_post -H 'Mcp-Method: tools/list')
assert_contains "missing _meta rejected" '-32602' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/list\",\"params\":{$META}}" | curl -sS --max-time 20 -X POST "$BASE" -H "$CT" -H 'Accept: application/json' -H "$PV" -H 'Mcp-Method: tools/list' --data-binary @-)
assert_contains "Accept without text/event-stream rejected" 'must accept' "$OUT"

OUT=$(printf '%s' 'this is not json{{' | mcp_post -H 'Mcp-Method: tools/list')
assert_not_contains "malformed JSON gets an error, not a hang" '"result"' "$OUT"
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\",\"params\":{$META}}" | mcp_post -H 'Mcp-Method: tools/list')
assert_contains "server alive after malformed JSON" '"tools"' "$OUT"

echo "== outbound HTTP (bridge) =="

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"http://127.0.0.1:${FIXTURE_PORT}/hello.txt\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get')
assert_contains "http_get fetches local fixture" 'hello from fixture' "$OUT"

# Regression: >4 KiB body of multibyte chars must truncate on a char boundary,
# not panic the instance.
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"http://127.0.0.1:${FIXTURE_PORT}/multibyte.txt\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get')
assert_contains "http_get truncates multibyte body without trapping" 'truncated' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"not a url\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get')
assert_contains "http_get invalid URL is a clean error" 'error' "$OUT"

# Black-holed upstream: must time out instead of wedging the instance.
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":16,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"http://127.0.0.1:${FIXTURE_PORT}/hang\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get')
assert_contains "http_get times out on black-holed upstream" 'timed out' "$OUT"
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":17,\"method\":\"tools/call\",\"params\":{\"name\":\"echo\",\"arguments\":{\"message\":\"alive after timeout\"},$META}}" | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: echo')
assert_contains "server alive after outbound timeout" 'alive after timeout' "$OUT"

echo "== robustness =="

# Oversized request body must be rejected without buffering it all.
OUT=$(python3 -c "import sys; sys.stdout.write('{\"padding\":\"' + 'x' * (5 * 1024 * 1024) + '\"}')" | mcp_post -H 'Mcp-Method: tools/list' -o /dev/null -w '%{http_code}')
case "$OUT" in
  413|400) pass "oversized body (5 MiB) rejected with $OUT" ;;
  *) fail "oversized body (5 MiB) rejected" "http status: $OUT" ;;
esac

CONCURRENT_FAILS=0
CONC_PIDS=()
for i in $(seq 1 8); do
  (printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":$i,\"method\":\"tools/call\",\"params\":{\"name\":\"add\",\"arguments\":{\"a\":$i,\"b\":1},$META}}" \
    | mcp_post -H 'Mcp-Method: tools/call' -H 'Mcp-Name: add' > "/tmp/mcp-e2e-conc-$i.json") &
  CONC_PIDS+=($!)
done
wait "${CONC_PIDS[@]}"
for i in $(seq 1 8); do
  grep -q '"sum"' "/tmp/mcp-e2e-conc-$i.json" || CONCURRENT_FAILS=$((CONCURRENT_FAILS + 1))
done
if [ "$CONCURRENT_FAILS" -eq 0 ]; then
  pass "8 concurrent tool calls all succeed"
else
  fail "8 concurrent tool calls all succeed" "$CONCURRENT_FAILS of 8 failed"
fi

echo "== Host-header guard (MCP_ALLOWED_HOSTS) =="

wasmtime serve -Sp3,cli,http --env "MCP_ALLOWED_HOSTS=127.0.0.1:${GUARD_PORT}" \
  --addr "127.0.0.1:${GUARD_PORT}" "$WASM" >/tmp/mcp-e2e-guard.log 2>&1 &
GUARD_PID=$!
for _ in $(seq 1 50); do
  curl -s -o /dev/null "http://127.0.0.1:${GUARD_PORT}/" && break
  sleep 0.2
done

OUT=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H "$CT" -H "$ACCEPT" -H "$PV" --data-binary @-)
assert_contains "allowed Host accepted" '"protocolVersion"' "$OUT"

OUT=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H 'Host: evil.example' -H "$CT" -H "$ACCEPT" -H "$PV" --data-binary @-)
assert_contains "disallowed Host rejected" 'Forbidden' "$OUT"

echo "== SSRF guard (default deny of local targets) =="

# The guard instance has no MCP_HTTP_GET_ALLOW_LOCAL: local targets denied.
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":13,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"http://127.0.0.1:${FIXTURE_PORT}/hello.txt\"},$META}}" \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H "$CT" -H "$ACCEPT" -H "$PV" -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get' --data-binary @-)
assert_contains "http_get denies loopback target by default" 'denied' "$OUT"

OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":14,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"http://169.254.169.254/latest/meta-data/\"},$META}}" \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H "$CT" -H "$ACCEPT" -H "$PV" -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get' --data-binary @-)
assert_contains "http_get denies link-local (IMDS) target" 'denied' "$OUT"

# file:// is refused either by the URI builder (invalid_params) or by the
# scheme check (denied) — both are denials; it must never produce a result.
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":15,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"file:///etc/passwd\"},$META}}" \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H "$CT" -H "$ACCEPT" -H "$PV" -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get' --data-binary @-)
assert_not_contains "http_get denies non-http scheme" '"isError":false' "$OUT"
OUT=$(printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":15,\"method\":\"tools/call\",\"params\":{\"name\":\"http_get\",\"arguments\":{\"url\":\"gopher://example.com/\"},$META}}" \
  | curl -sS --max-time 20 -X POST "http://127.0.0.1:${GUARD_PORT}/" -H "$CT" -H "$ACCEPT" -H "$PV" -H 'Mcp-Method: tools/call' -H 'Mcp-Name: http_get' --data-binary @-)
assert_contains "http_get denies non-http scheme with host" 'denied' "$OUT"

echo
echo "== results: ${PASS} passed, ${FAIL} failed =="
if [ "$FAIL" -gt 0 ]; then
  printf 'failed: %s\n' "${FAILED_NAMES[@]}"
  exit 1
fi
