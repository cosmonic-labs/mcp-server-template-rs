# mcp-server-template-rs

Template for building **Model Context Protocol (MCP) servers** as
**WebAssembly components** that deploy on [Cosmonic Desktop]
(wasmCloud v2) — see the [Cosmonic Desktop docs].

- **Official SDK**: [`rmcp`] 3.x (the official MCP Rust SDK), speaking the
  [2026-07-28 MCP specification] — stateless protocol core, header-based
  routing (`Mcp-Method` / `Mcp-Name`), structured tool output.
- **WASI p3**: exports [`wasi:http/handler@0.3.0`] — the async WASI 0.3 HTTP
  interface — so the runtime can invoke instances concurrently and scale them
  per request.
- **Stateless by design**: no sessions, no affinity. Every request is
  self-contained, matching both the 2026-07-28 spec and the Wasm
  instance-per-request execution model.
- **Streaming responses**: headers return as soon as rmcp produces them and
  body frames — including SSE events from long-running tools — stream across
  the executor boundary as they materialize.
- **Outbound HTTP for tools**: `bridge::outbound::fetch` performs requests
  directly over the `wasi:http@0.3.0` client bindings, governed by the
  workload's `allowedHosts` policy (deny-all by default).
- **Enterprise observability**: `tracing` spans with structured JSON logs,
  W3C `traceparent` correlation, OTLP export via the host, and an opt-in
  `wasi-otel` feature for native host-joined traces (see
  [Observability](#observability)).

## Layout

```
├── .cargo/config.toml   # default target wasm32-wasip2 + tokio_unstable cfg
├── .wash/config.yaml    # wash v2 / Cosmonic Desktop project config
├── deploy/workload.yaml # deploy manifest for the published image (mcp.ai labels)
├── docs/auth.md         # authorization options for the Desktop use-case
├── src/
│   ├── lib.rs           # wasi:http/handler export, streaming response pump
│   ├── bridge.rs        # tokio ↔ component-model-async bridge + outbound HTTP
│   ├── server.rs        # ServerHandler + your tools — start here
│   └── telemetry.rs     # tracing/OTEL wiring
└── workload.yaml        # local-dev Workload manifest (built-in registry)
```

## Prerequisites

- Rust 1.90+ with the wasip2 target: `rustup target add wasm32-wasip2`
- To deploy: [Cosmonic Desktop] (runtime ≥ 2.5) — install and setup per the
  [Cosmonic Desktop docs]

## Scaffold

Start a new server from this template with `wash` (v2 syntax — the git URL is
the positional argument; wash 2.x removed the old `--git` flag):

```console
$ wash new https://github.com/cosmonic-labs/mcp-server-template-rs --name my-mcp-server
$ cd my-mcp-server
```

Then rename the `mcp-server-template` / `mcp-server` occurrences for your
server: the `Cargo.toml` package name, `.wash/config.yaml`
`build.component_path`, and in both workload manifests the `metadata.name`,
ingress `host`, `mcp.ai/*` labels, `image` ref, and `MCP_ALLOWED_HOSTS`.

## Build

```console
$ cargo build --release
```

`.cargo/config.toml` defaults the target to `wasm32-wasip2`, which emits a
component directly. The output at
`target/wasm32-wasip2/release/mcp_server_template.wasm` exports
`wasi:http/handler@0.3.0` (verify with `wasm-tools component wit <path>`).

## Deploy on Cosmonic Desktop

Deployment is via [Cosmonic Desktop] — see the [Cosmonic Desktop docs] for
installation and concepts. In brief:

1. **Register** the project with the daemon (or open it in the Desktop UI) —
   the project config is `.wash/config.yaml`.
2. **Promote** — builds the component and pushes it to Desktop's built-in
   registry, returning a **digest-pinned image reference**.
3. **Apply** `workload.yaml` with that image reference (Desktop UI, the
   `cosmonic_apply_workload` MCP tool, or `POST /v1/workloads`). The manifest
   already routes ingress by Host header (`mcp-server.localhost`) and sets
   `MCP_ALLOWED_HOSTS` to match.

For a published image (a public OCI registry rather than a locally promoted
build), use [`deploy/workload.yaml`](deploy/workload.yaml) instead. Both
manifests carry the `mcp.ai/*` labels and the
`desktop.cosmonic.com/source: mcp` annotation — Cosmonic Desktop detects
these to catalog the workload as an MCP server and drive MCP-specific
behavior (listing, client connection, transport/auth handling); the deploy
manifest documents what each label means. Keep them accurate when you fork.

### Talk to it

Initialize (on Cosmonic Desktop the ingress routes by Host header):

```console
$ curl -X POST http://mcp-server.localhost:8200/ \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

Every request after `initialize` follows the 2026-07-28 stateless
conventions: an `Mcp-Method` routing header (plus `Mcp-Name` for tool calls),
and a per-request `_meta` in params carrying **both**
`io.modelcontextprotocol/protocolVersion` and
`io.modelcontextprotocol/clientCapabilities`. Omit the header or either
`_meta` field and the server answers with JSON-RPC `-32602` — any conformant
streamable-HTTP client sends all of this automatically; only hand-written
requests (curl, scripts) need to supply it themselves. List the tools:

```console
$ curl -X POST http://mcp-server.localhost:8200/ \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' \
    -H 'Mcp-Method: tools/list' \
    -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
```

Call a tool:

```console
$ curl -X POST http://mcp-server.localhost:8200/ \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' \
    -H 'Mcp-Method: tools/call' -H 'Mcp-Name: add' \
    -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"add","arguments":{"a":1.5,"b":2.5},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
data: {"jsonrpc":"2.0","id":3,"result":{"resultType":"complete","content":[{"type":"text","text":"{\"sum\":4.0}"}],"structuredContent":{"sum":4.0},"isError":false}}
```

Responses arrive SSE-framed (`Content-Type: text/event-stream`, `data:`
lines) — the transport's default, which lets long-running tools stream
progress. Any MCP client that speaks streamable HTTP handles this (and the
headers above) for you. Only POST is served: GET/DELETE return 405, and there
are deliberately no CORS headers — browser pages cannot call the server
cross-origin, which is part of the DNS-rebinding defense.

## Writing your own tools

Edit `src/server.rs`. Each tool is an async method with a
`Parameters<T>` argument; `T` derives `Deserialize` + `JsonSchema` and becomes
the tool's input schema automatically:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GreetParams {
    /// Who to greet.
    pub name: String,
}

#[tool(description = "Greet someone by name")]
async fn greet(&self, Parameters(p): Parameters<GreetParams>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(format!("Hello, {}!", p.name))]))
}
```

Rules of the road:

- **State**: instances are ephemeral and stateless. Persist through a host
  capability (`wasi:keyvalue`, a database over HTTP, …), never in memory.
- **Outbound calls**: use `bridge::outbound::fetch` (see the `http_get`
  example tool) — it goes directly over the `wasi:http@0.3.0` client
  bindings. Add every host you dial to `allowedHosts` in the workload; the
  default is deny-all and a blocked host surfaces as
  `ErrorCode::HttpRequestDenied`.
- **The two async worlds** (see `src/bridge.rs`): WASI p3 I/O runs under the
  component-model executor; rmcp dispatch (and your tools) run inside a
  single-threaded tokio runtime. Never await a WASI p3 future from tool code
  directly — go through the bridge, which services outbound requests while
  tool code is suspended. Authorization options are sketched in
  [docs/auth.md](docs/auth.md).

## Observability

The template follows the wasmCloud / Cosmonic observability model: **the host
owns OTLP export; the component emits structured, correlatable signals.**

- All logging goes through [`tracing`]; `src/telemetry.rs` installs a JSON
  formatter on **stderr** (stdout is never used for logs). Log level comes
  from `RUST_LOG`.
- Every request runs in an `mcp.request` span carrying `http.method`,
  `http.path`, `mcp.method`, and the W3C `traceparent` header when the
  ingress/host propagates one — so log records can be joined with platform
  traces in your collector.
- Each tool adds its own span (`tool.echo`, `tool.add`, …) via
  `#[tracing::instrument]`. Do the same in your tools.
- On wasmCloud hosts, enable OTLP export with
  `WASMCLOUD_OBSERVABILITY_ENABLED=true` and `OTEL_EXPORTER_OTLP_ENDPOINT`;
  on Cosmonic Control, workload telemetry is on by default.
- **Native `wasi:otel`** (`cargo build --features wasi-otel`): spans stream to
  the host via `wasi:otel@0.2.0-rc.2` (through
  [`opentelemetry-wasi`](https://github.com/bytecodealliance/opentelemetry-wasi))
  and the request span is parented on the host's active span, joining host
  traces natively. Requires a host implementing a *matching* `wasi:otel`
  revision — as of 2026-08-07 the rc.2 WITs in opentelemetry-wasi and
  wash-runtime have drifted (`tracing.on-start` signature), so this feature
  fails to link on Cosmonic Desktop 0.5.18 until the two align. Keep it off
  for `wasmtime serve` (no wasi:otel host support there).

## Testing

`scripts/e2e.sh` builds the component and runs the full protocol suite
against it under `wasmtime serve` — spec conformance, streaming, the outbound
bridge (against a local fixture server, including a black-holed upstream),
the Host-header guard, the SSRF guard, oversized bodies, and concurrency. CI
runs it on every push. Note that `.cargo/config.toml` defaults the build
target to wasm32-wasip2, so plain `cargo test` would build (unrunnable) wasm
test binaries — the e2e harness is the test entry point.

## Configuration

| Env var | Default | Purpose |
|---|---|---|
| `RUST_LOG` | `info` | Log filter (`tracing_subscriber::EnvFilter` syntax). Avoid `trace` in production: rmcp logs full request payloads at trace level. |
| `MCP_ALLOWED_HOSTS` | unset → localhost only | DNS-rebinding guard: comma-separated `Host` values to accept (`host` matches any port, `host:port` is exact, `*` disables the guard). Unset keeps rmcp's safe default (localhost/127.0.0.1/::1). **Deployments reached under another name must set this** — e.g. `mcp-server.localhost` on Cosmonic Desktop, as `workload.yaml` does. |
| `MCP_HTTP_GET_ALLOW_LOCAL` | unset (deny) | Lets the `http_get` example tool target loopback/private/link-local addresses (development only). |
| `MCP_OUTBOUND_TIMEOUT_MS` | `30000` | Deadline for one outbound `bridge::outbound::fetch` exchange. |
| `MCP_OUTBOUND_MAX_BYTES` | `4194304` (4 MiB) | Upper bound on a buffered outbound response body; raise it for upstreams that return larger documents. |

## License

Apache-2.0

[Cosmonic Desktop]: https://cosmonic.com
[Cosmonic Desktop docs]: https://cosmonic.com/docs/desktop
[`rmcp`]: https://github.com/modelcontextprotocol/rust-sdk
[2026-07-28 MCP specification]: https://blog.modelcontextprotocol.io/posts/2026-07-28/
[`wasi:http/handler@0.3.0`]: https://github.com/WebAssembly/wasi-http
[`tracing`]: https://docs.rs/tracing
