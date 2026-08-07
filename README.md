# mcp-server-template-rs

Template for building **Model Context Protocol (MCP) servers** as
**WebAssembly components** that run on [Cosmonic Desktop] and any
wasmCloud v2 / Wasmtime 46+ runtime.

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
├── docs/auth.md         # authorization options for the Desktop use-case
├── src/
│   ├── lib.rs           # wasi:http/handler export, streaming response pump
│   ├── bridge.rs        # tokio ↔ component-model-async bridge + outbound HTTP
│   ├── server.rs        # ServerHandler + your tools — start here
│   └── telemetry.rs     # tracing/OTEL wiring
└── workload.yaml        # Cosmonic Workload manifest (wasmCloud v2 CRD)
```

## Prerequisites

- Rust 1.90+ with the wasip2 target: `rustup target add wasm32-wasip2`
- To run locally: [Cosmonic Desktop] (runtime ≥ 2.5), or `wasmtime` ≥ 46

## Build

```console
$ cargo build --release
```

`.cargo/config.toml` defaults the target to `wasm32-wasip2`, which emits a
component directly. The output at
`target/wasm32-wasip2/release/mcp_server_template.wasm` exports
`wasi:http/handler@0.3.0` (verify with `wasm-tools component wit <path>`).

## Run

### Cosmonic Desktop

Open the project in Cosmonic Desktop (or register it with the daemon), then
promote and apply `workload.yaml` with the digest-pinned image ref from
promote. Or with wasmtime:

```console
$ wasmtime serve -Sp3,cli,http target/wasm32-wasip2/release/mcp_server_template.wasm
```

### Talk to it

Initialize (on Cosmonic Desktop the ingress routes by Host header):

```console
$ curl -X POST http://mcp-server.localhost:8200/ \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

Call a tool — note the 2026-07-28 conventions: routing headers and per-request
`_meta` instead of a session:

```console
$ curl -X POST http://mcp-server.localhost:8200/ \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' \
    -H 'Mcp-Method: tools/call' -H 'Mcp-Name: add' \
    -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"add","arguments":{"a":1.5,"b":2.5},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
{"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","content":[{"type":"text","text":"{\"sum\":4.0}"}],"structuredContent":{"sum":4.0},"isError":false}}
```

Any MCP client that speaks streamable HTTP works too (they send these
headers for you).

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

## Configuration

| Env var | Default | Purpose |
|---|---|---|
| `RUST_LOG` | `info` | Log filter (`tracing_subscriber::EnvFilter` syntax) |
| `MCP_ALLOWED_HOSTS` | unset (guard off) | Comma-separated exact `host[:port]` values accepted in the `Host` header (DNS-rebinding guard). Leave unset behind Host-routed ingress; set it when exposing the server on a routable origin. |

## License

Apache-2.0

[Cosmonic Desktop]: https://cosmonic.com
[`rmcp`]: https://github.com/modelcontextprotocol/rust-sdk
[2026-07-28 MCP specification]: https://blog.modelcontextprotocol.io/posts/2026-07-28/
[`wasi:http/handler@0.3.0`]: https://github.com/WebAssembly/wasi-http
[`tracing`]: https://docs.rs/tracing
