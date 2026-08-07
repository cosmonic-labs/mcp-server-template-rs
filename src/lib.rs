//! MCP server template for Cosmonic Desktop.
//!
//! This component exports [`wasi:http/handler@0.3.0`] (WASI p3) and serves the
//! Model Context Protocol over the streamable HTTP transport from the official
//! [`rmcp`] SDK, in the stateless mode introduced by the 2026-07-28 MCP
//! specification.
//!
//! Responses stream: headers are returned to the host as soon as `rmcp`
//! produces them, and body frames (including SSE events from long-running
//! tools) are pumped to the `wasi:http` body stream as they materialize. See
//! [`bridge`] for how the tokio and component-model async worlds interlock,
//! and [`bridge::outbound`] for how tool code performs outbound HTTP through
//! the `wasi:http` client bindings.
//!
//! [`wasi:http/handler@0.3.0`]: https://github.com/WebAssembly/wasi-http

pub mod bridge;
mod server;
mod telemetry;

use std::pin::Pin;
use std::sync::Arc;

use http_body::Body;
use http_body_util::{BodyExt, Full};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use wasip3::http::types::{ErrorCode, Request, Response};
use wasip3::http_compat::{http_from_wasi_request, BodyWriter};

struct Component;

wasip3::http::service::export!(Component);

impl wasip3::exports::http::handler::Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        telemetry::init();

        // One exchange at a time per instance: the bridge can only drive one
        // tokio-world computation at once. Hosts scale out with more
        // instances, not intra-instance concurrency.
        let guard = bridge::request_lock().lock_owned().await;

        let request = http_from_wasi_request(request)?;
        let (parts, body) = request.into_parts();

        // Buffer the request body in component-model context: an MCP request
        // is a single JSON-RPC message (rmcp enforces a max size, 4 MiB by
        // default). Responses, by contrast, stream.
        let bytes = body
            .collect()
            .await
            .map_err(|err| ErrorCode::InternalError(Some(format!("failed to read body: {err}"))))?
            .to_bytes();
        let request = http::Request::from_parts(parts, Full::new(bytes));

        let span = telemetry::request_span(&request);
        let service = StreamableHttpService::new(
            || Ok(server::TemplateServer::new()),
            Arc::new(NeverSessionManager::default()),
            transport_config(),
        );

        // Run rmcp dispatch until response headers are available. Tool code
        // runs in here; any outbound HTTP it submits is serviced by the
        // bridge along the way.
        let response = bridge::drive(tracing::Instrument::instrument(
            service.handle(request),
            span.clone(),
        ))
        .await;
        let (parts, mut body) = response.into_parts();

        // Hand the host a streaming response and pump rmcp's body frames from
        // a spawned component-model task: SSE events flow out as tools make
        // progress, and outbound requests keep being serviced until the body
        // completes.
        let headers = parts
            .headers
            .try_into()
            .map_err(|err| ErrorCode::InternalError(Some(format!("invalid headers: {err}"))))?;
        let (mut writer, body_rx, result_rx) = BodyWriter::new();
        let (wasi_response, _transmit) = Response::new(headers, Some(body_rx), result_rx);
        wasi_response
            .set_status_code(parts.status.as_u16())
            .map_err(|()| ErrorCode::InternalError(Some("invalid status code".into())))?;

        wasip3::wit_bindgen::spawn(async move {
            let _guard = guard;
            loop {
                let frame = bridge::drive(tracing::Instrument::instrument(
                    std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)),
                    span.clone(),
                ))
                .await;
                match frame {
                    Some(Ok(frame)) => {
                        if writer.send_frame(frame).await.is_err() {
                            // Peer dropped the read end; stop pumping.
                            return;
                        }
                    }
                    // The transport's body error type is Infallible.
                    Some(Err(never)) => match never {},
                    None => break,
                }
            }
            let trailers = std::mem::take(&mut writer.trailers);
            let trailers = (!trailers.is_empty())
                .then(|| trailers.try_into().ok())
                .flatten();
            drop(writer.stream_writer);
            let _ = writer.result_writer.write(Ok(trailers)).await;
        });

        Ok(wasi_response)
    }
}

/// Transport configuration: fully stateless per the 2026-07-28 spec.
///
/// The 2026-07-28 MCP specification makes the protocol core stateless
/// (SEP-2567); disabling legacy session mode extends the same behavior to
/// clients speaking older protocol revisions, which lets this component scale
/// horizontally with no session affinity. Everything else keeps the SDK's
/// defaults — in particular responses use SSE streaming when the client
/// accepts it.
fn transport_config() -> StreamableHttpServerConfig {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
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
