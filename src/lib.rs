//! MCP server template for Cosmonic Desktop.
//!
//! This component exports [`wasi:http/handler@0.3.0`] (WASI p3) and serves the
//! Model Context Protocol over the streamable HTTP transport from the official
//! [`rmcp`] SDK, in the stateless mode introduced by the 2026-07-28 MCP
//! specification.
//!
//! Architecture: there are two async worlds in this component and one bridge
//! between them.
//!
//! 1. The **component-model async** world (outer): the `handle` export is
//!    driven by the host through the WASI p3 async ABI. All request/response
//!    body streaming happens here.
//! 2. The **tokio** world (inner): `rmcp`'s dispatch machinery uses tokio
//!    primitives, so protocol handling runs to completion on a single-threaded
//!    tokio runtime via `block_on`. Only pure compute and tokio-native I/O may
//!    run here — never `await` a WASI p3 future inside `block_on`, it will
//!    deadlock (the host cannot make progress while the export is blocked).
//!
//! [`wasi:http/handler@0.3.0`]: https://github.com/WebAssembly/wasi-http

mod server;
mod telemetry;

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use wasip3::http::types::{ErrorCode, Request, Response};
use wasip3::http_compat::{http_from_wasi_request, http_into_wasi_response};

struct Component;

wasip3::http::service::export!(Component);

impl wasip3::exports::http::handler::Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        telemetry::init();

        let request = http_from_wasi_request(request)?;
        let (parts, body) = request.into_parts();

        // Read the full request body while still in the component-model async
        // context. MCP requests are single JSON-RPC messages, so buffering is
        // appropriate; rmcp enforces a max body size (default 4 MiB).
        let bytes = body
            .collect()
            .await
            .map_err(|err| ErrorCode::InternalError(Some(format!("failed to read body: {err}"))))?
            .to_bytes();

        let request = http::Request::from_parts(parts, Full::new(bytes));
        let response = tokio_runtime().block_on(serve_mcp(request));

        http_into_wasi_response(response)
    }
}

/// Handles one MCP-over-HTTP exchange. Runs entirely inside the tokio runtime.
async fn serve_mcp(request: http::Request<Full<Bytes>>) -> http::Response<Full<Bytes>> {
    let service = StreamableHttpService::new(
        || Ok(server::TemplateServer::new()),
        Arc::new(NeverSessionManager::default()),
        transport_config(),
    );

    let span = telemetry::request_span(&request);
    let response = tracing::Instrument::instrument(service.handle(request), span).await;

    // Buffer the response body inside the tokio runtime: in stateless JSON
    // mode every response body is finite.
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        // The transport's body error type is Infallible.
        Err(never) => match never {},
    };
    http::Response::from_parts(parts, Full::new(bytes))
}

/// Transport configuration: fully stateless, JSON responses.
///
/// The 2026-07-28 MCP specification makes the protocol core stateless
/// (SEP-2567); disabling legacy session mode extends the same behavior to
/// clients speaking older protocol revisions, which lets this component scale
/// horizontally with no session affinity.
fn transport_config() -> StreamableHttpServerConfig {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    // Prefer plain `application/json` responses over SSE streams. This keeps
    // response bodies finite (required by the buffering in `serve_mcp`).
    config.json_response = true;
    // DNS-rebinding guard. rmcp's default only allows a `Host` of
    // localhost/127.0.0.1/::1, but Cosmonic's ingress routes requests to this
    // workload BY Host header (e.g. `mcp-server.localhost`) — only traffic
    // addressed to this workload ever reaches it — so the guard is disabled
    // unless `MCP_ALLOWED_HOSTS` (comma-separated, exact `host[:port]`
    // matches) is set. Set it when exposing the server on a routable origin.
    config.allowed_hosts = std::env::var("MCP_ALLOWED_HOSTS")
        .map(|hosts| {
            hosts
                .split(',')
                .map(|host| host.trim().to_owned())
                .filter(|host| !host.is_empty())
                .collect()
        })
        .unwrap_or_default();
    config
}

/// Lazily-constructed single-threaded tokio runtime.
///
/// WASI has no threads, so this is a `current_thread` runtime; `block_on`
/// also drives any tasks spawned with `tokio::spawn`.
fn tokio_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .expect("failed to build single-threaded tokio runtime")
    })
}
