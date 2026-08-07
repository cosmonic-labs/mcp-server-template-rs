//! The MCP server implementation.
//!
//! Replace the example tools in this module with your own. The `#[tool]` /
//! `#[tool_router]` / `#[tool_handler]` macros from `rmcp` generate the JSON
//! schema for each tool from its `Parameters` type and wire up dispatch.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

/// The MCP server for this component. One instance is created per request —
/// the transport is stateless (2026-07-28 spec), so do not keep per-session
/// state on this struct. Durable state belongs in a host capability such as
/// `wasi:keyvalue`.
#[derive(Clone)]
pub struct TemplateServer {
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EchoParams {
    /// The message to echo back.
    pub message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddParams {
    /// Left operand.
    pub a: f64,
    /// Right operand.
    pub b: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HttpGetParams {
    /// URL to fetch. Its host must be on the workload's outbound
    /// `allowedHosts` allow-list (deny-all by default).
    pub url: String,
}

/// Response bodies larger than this are truncated in `http_get` output.
const HTTP_GET_BODY_LIMIT: usize = 4096;

#[tool_router]
impl TemplateServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Example tool: echoes the provided message back to the client.
    #[tool(description = "Echo a message back to the caller")]
    #[tracing::instrument(name = "tool.echo", skip(self))]
    async fn echo(
        &self,
        Parameters(params): Parameters<EchoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            params.message,
        )]))
    }

    /// Example tool: adds two numbers, demonstrating structured output
    /// (`structuredContent` alongside human-readable text).
    #[tool(description = "Add two numbers and return the sum")]
    #[tracing::instrument(name = "tool.add", skip(self))]
    async fn add(
        &self,
        Parameters(params): Parameters<AddParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let sum = params.a + params.b;
        Ok(CallToolResult::structured(
            serde_json::json!({ "sum": sum }),
        ))
    }

    /// Example tool: outbound HTTP through the `wasi:http@0.3.0` client
    /// bindings (see [`crate::bridge::outbound`]). The workload's
    /// `allowedHosts` policy governs which hosts are reachable.
    #[tool(
        description = "HTTP GET a URL (host must be on the workload's outbound allow-list); \
                          returns the status line and up to 4 KiB of the body"
    )]
    #[tracing::instrument(name = "tool.http_get", skip(self))]
    async fn http_get(
        &self,
        Parameters(params): Parameters<HttpGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let request = http::Request::get(&params.url)
            .body(bytes::Bytes::new())
            .map_err(|err| ErrorData::invalid_params(err.to_string(), None))?;
        match crate::bridge::outbound::fetch(request).await {
            Ok(response) => {
                let status = response.status();
                let mut body = String::from_utf8_lossy(response.body()).into_owned();
                if body.len() > HTTP_GET_BODY_LIMIT {
                    body.truncate(HTTP_GET_BODY_LIMIT);
                    body.push_str("…[truncated]");
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "HTTP {status}\n{body}"
                ))]))
            }
            // Tool-level error: the caller should see why the fetch failed.
            Err(err) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "fetch failed: {err}"
            ))])),
        }
    }

    /// Example tool: reports the current wall-clock time from the WASI host.
    #[tool(description = "Get the current UTC time as a unix timestamp in milliseconds")]
    #[tracing::instrument(name = "tool.current_time", skip(self))]
    async fn current_time(&self) -> Result<CallToolResult, ErrorData> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| ErrorData::internal_error(err.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            now.as_millis().to_string(),
        )]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TemplateServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Template MCP server running as a WebAssembly component on \
                 Cosmonic Desktop. Use `echo`, `add`, or `current_time` to \
                 verify connectivity, `http_get` to test outbound HTTP, then \
                 replace them with your own tools.",
            )
    }
}
