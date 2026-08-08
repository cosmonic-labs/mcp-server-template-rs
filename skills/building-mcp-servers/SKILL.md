---
name: building-mcp-servers
description: Build, test, and deploy an MCP server as a WebAssembly component on Cosmonic Desktop using the mcp-server-template-rs framework (rmcp 3.x, wasi:http@0.3.0, MCP spec 2026-07-28). Use when creating a new MCP server, adding tools to one, or deploying/debugging one on Cosmonic Desktop or wasmtime.
---

# Building MCP servers with mcp-server-template-rs

Version: 1 (updated after: template hardening review)

This skill turns a tool idea into a deployed, spec-compliant MCP server
component. Follow the phases in order; the Pitfalls section at the end is a
living list — read it BEFORE writing code, not after something breaks.

## Non-negotiable guardrails

1. **Spec**: MCP 2026-07-28 (stateless core, `Mcp-Method`/`Mcp-Name` header
   routing, per-request `_meta`, `resultType: complete`, structured output).
   The template's transport config already enforces this — do not change
   `legacy_session_mode`, the session manager, or protocol pinning.
2. **Target**: the component must export `wasi:http/handler@0.3.0` (WASI p3),
   built on `wasm32-wasip2` with the template's `.cargo/config.toml` intact.
3. **Cosmonic Desktop compatibility**: deploys as a Workload with the
   `interfaces: ["handler"]` hostInterface (p3 name — NOT `incoming-handler`),
   `mcp.ai/*` labels, `MCP_ALLOWED_HOSTS` matching the ingress host, and
   fail-closed `allowedHosts`.
4. **Never** await a WASI p3 future from tool code. Outbound HTTP goes
   through `crate::bridge::outbound::fetch` only.
5. Every server ships with a passing `scripts/e2e.sh` adapted to its tools.

## Phase 1 — scaffold

Copy the template (do not `git clone` into the new project):

```bash
SRC=<path-to-mcp-server-template-rs>
mkdir -p <project> && cd <project>
cp -R $SRC/.cargo $SRC/.wash $SRC/src $SRC/scripts $SRC/Cargo.toml $SRC/Cargo.lock \
      $SRC/workload.yaml $SRC/.gitignore .
```

Then rename everywhere, keeping the three names aligned:
- `Cargo.toml` `[package] name` (kebab-case, e.g. `sec-edgar-mcp`)
- `.wash/config.yaml` `build.component_path` → `target/wasm32-wasip2/release/<name_with_underscores>.wasm`
- `scripts/e2e.sh` `WASM=` path
- `workload.yaml`: `metadata.name`, hostInterface `config.host`
  (`<name>.localhost`), labels (`app.kubernetes.io/name`, `mcp.ai/domain`),
  and env `MCP_ALLOWED_HOSTS` = the same `<name>.localhost`.

`mcp.ai` label conventions (catalogued by Cosmonic Desktop):
`auth-type: none|oauth`, `domain: <subject-area>`, `function-type: tools`,
`spec-version: "2026-07-28"`, `statefulness: stateless`,
`transport: streamable-http`, `sandbox-isolation: wasmtime`,
`agent-access: internal-only`.

## Phase 2 — implement tools (src/server.rs)

Replace the example tools. Patterns:

- Params: a `#[derive(Debug, Deserialize, JsonSchema)]` struct per tool with
  doc comments on every field (they become the client-visible schema).
- Simple result: `CallToolResult::success(vec![ContentBlock::text(..)])`.
- Machine-readable result: `CallToolResult::structured(serde_json::json!({..}))`
  — prefer this whenever the output has shape; it fills both
  `structuredContent` and a text fallback.
- **Error discipline**: `Err(ErrorData::invalid_params(..))` ONLY for
  requests the server cannot route/parse; `Ok(CallToolResult::error(..))`
  for "the tool ran and failed" — the caller sees your message.
- Add `#[tracing::instrument(name = "tool.<name>", skip(self))]` on every
  tool.
- Update `get_info()` instructions to describe the server's purpose and
  tools.
- No per-session state on the server struct: instances are ephemeral.
  Durable state → host capability or upstream API.

Outbound HTTP from a tool:

```rust
let request = http::Request::get(url).body(bytes::Bytes::new())
    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
let response = crate::bridge::outbound::fetch(request).await; // 30s deadline, 4MiB cap
```

- Always set required upstream headers on the request (e.g. SEC EDGAR
  requires a descriptive `User-Agent`).
- Keep the `deny_private_target` guard wired for any tool that accepts a
  client-supplied URL. Tools that call a FIXED upstream host don't need it
  (the host either is or isn't in `allowedHosts`).
- Every upstream host must be listed in `workload.yaml` `allowedHosts` (and
  the `.wash/config.yaml` workload section). Empty list = deny-all.
- Secrets (API keys): read from env (`std::env::var`); in the Workload use
  `secretFrom` + `cosmonic_set_secret` for production, plain env config only
  for examples — and say so in the README.

## Phase 3 — compile gates

```bash
cargo fmt && cargo clippy --all-features -- -D warnings && cargo build --release
wasm-tools component wit target/wasm32-wasip2/release/<name>.wasm | grep 'export wasi:http/handler@0.3.0'
```

`cargo test` does NOT work here (build target is wasm) — `scripts/e2e.sh` is
the test entry point.

## Phase 4 — adapt and run the e2e suite

Edit `scripts/e2e.sh`:
- Update `WASM=` and pick unique ports (avoid 8199/8198/8197 if another
  server's suite may run concurrently).
- Replace the example-tool cases (echo/add/current_time/http_get) with cases
  for YOUR tools: happy path, malformed params, and at least one
  domain-specific adversarial case per tool (boundary values, huge inputs,
  unicode, injection-shaped strings).
- Keep the protocol/spec-enforcement/robustness/guard sections — they test
  the framework and must keep passing.
- For tools calling a fixed upstream: add a local fixture server (see the
  python fixture in the template's e2e) and, where the real API is keyless
  and cheap, one live smoke case. Wasmtime's `-Shttp` has no outbound
  allow-list, so fixtures work without policy changes.

Run: `scripts/e2e.sh` — all cases green before proceeding.

## Phase 5 — deploy to Cosmonic Desktop and verify

The daemon socket is `~/Library/Application Support/Cosmonic/cosmonicd.sock`.

```bash
SOCK="$HOME/Library/Application Support/Cosmonic/cosmonicd.sock"
# 1. Register the project directory (needs .wash/config.yaml):
curl -sS --unix-socket "$SOCK" -X POST http://localhost/v1/projects \
  -H 'Content-Type: application/json' -d '{"path":"<absolute project path>"}'
# 2. Promote (build+push). NOTE: the cosmonic_promote MCP tool sends the
#    wrong field name (`reference` vs `ref`) — use the socket API:
curl -sS --unix-socket "$SOCK" -X POST http://localhost/v1/projects/<id>/promote \
  -H 'Content-Type: application/json' \
  -d '{"ref":"oci-registry.localhost:8200/<name>:<version>","insecure":true}'
# returns a digest-pinned image ref — use THAT in the workload.
```

Apply the workload with the `cosmonic_apply_workload` MCP tool (or POST
/v1/workloads), with the digest-pinned image. Then verify through the
ingress (routes by Host header):

```bash
curl -sS -X POST -H 'Host: <name>.localhost' http://127.0.0.1:8200/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'MCP-Protocol-Version: 2026-07-28' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}'
```

Must return `data: {...  "protocolVersion":"2026-07-28" ...}`. Then exercise
one real tool call the same way (add `Mcp-Method: tools/call`,
`Mcp-Name: <tool>`, and `"_meta":{"io.modelcontextprotocol/protocolVersion":
"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}` in params).

Local testing without Desktop: `wasmtime serve -Sp3,cli,http --addr
127.0.0.1:<port> <wasm>` — ALWAYS pass `--addr 127.0.0.1` (default binds
0.0.0.0).

## Phase 6 — README

Every server gets a README: what it does, tool table (name, params, output),
env vars, `allowedHosts` needed, build/run/deploy commands (both wasmtime and
Cosmonic Desktop), and example curl calls.

## Pitfalls (living list — add what bites you)

- **rmcp 3.x API**: it's `ContentBlock` (not `Content`); `ServerInfo`/
  `Implementation`/`CallToolResult` are `#[non_exhaustive]` — use
  constructors/builders (`ServerInfo::new(..).with_server_info(..)
  .with_instructions(..)`). `#[tool_handler]` defaults to `Self::tool_router()`
  (rebuild per call); pass `#[tool_handler(router = self.tool_router)]` to
  use the field.
- **Never `String::truncate` at a byte offset** on upstream/user content —
  panics mid-UTF-8-char, and panic=abort kills the whole instance. Use the
  template's `truncation_boundary`.
- **Bound every buffer**: request bodies, outbound response bodies, anything
  collected from a stream (`http_body_util::Limited`). The template does
  this for the transport; do it for anything you add.
- **Deadline every await that a peer controls** — the bridge gives outbound
  fetch a 30s deadline; if you add other host I/O, race it with
  `bridge::timeout`.
- **tokio::select in shell scripts**: a bare `wait` in e2e scripts waits on
  the background *servers* too — always `wait "${PIDS[@]}"`.
- **wasmtime flags**: `-Sp3,cli,http` (p3 alone fails: missing wasi:cli;
  without `http` outbound client import fails to link).
- **Host guard vs ingress**: unset `MCP_ALLOWED_HOSTS` = localhost-only →
  Desktop ingress requests (Host `<name>.localhost`) get 403. Set it in the
  workload env. `host` entry matches any port; `host:port` exact; `*`
  disables.
- **wasi-otel feature**: leave OFF for wasmtime and Desktop 0.5.x — the
  rc.2 WIT signatures currently mismatch (`tracing.on-start`), instantiation
  fails. Baseline stderr JSON logging always works.
- **Desktop workload names**: never reuse/overwrite an existing workload
  name — list first (`cosmonic_list_workloads`), pick a fresh name.
- **Registry addressing**: the built-in store is
  `oci-registry.localhost:8200/...` with `insecure: true`; bare refs go to
  docker.io and fail.
- **Big upstream JSON**: outbound responses are capped at 4 MiB — pick
  API endpoints that return bounded payloads (e.g. prefer per-concept over
  full-dump endpoints) and paginate/limit where the API supports it.
