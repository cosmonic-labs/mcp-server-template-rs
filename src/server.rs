//! The MCP server implementation.
//!
//! Replace the example tools in this module with your own. The `#[tool]` /
//! `#[tool_router]` / `#[tool_handler]` macros from `rmcp` generate the JSON
//! schema for each tool from its `Parameters` type and wire up dispatch.
//!
//! Alongside the tools, this server publishes **skills** — natural-language
//! playbooks served over the MCP resources primitive under `skill://` URIs.
//! See [`crate::skills`]; the handlers at the bottom of this file are the
//! protocol surface for them.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::skills;

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

    /// Names of the tools this server exposes, read off the generated router
    /// so the discovery document (see [`crate::discovery`]) cannot drift from
    /// what `tools/list` actually returns.
    pub fn tool_names() -> Vec<String> {
        Self::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
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
    /// bindings (see [`crate::bridge::outbound`]).
    ///
    /// Two layers of policy apply: on wasmCloud/Cosmonic the workload's
    /// `allowedHosts` list is enforced by the host (deny-all by default), and
    /// [`deny_private_target`] refuses loopback/private/link-local targets
    /// in-guest as defense in depth — runtimes like `wasmtime serve -Shttp`
    /// apply no outbound filtering at all, and without this check any client
    /// could probe internal services through the tool (SSRF). Set
    /// `MCP_HTTP_GET_ALLOW_LOCAL=true` to permit local targets (development).
    #[tool(
        description = "HTTP GET a URL (subject to the outbound allow-list policy); \
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
        if let Some(reason) = deny_private_target(request.uri()) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(reason)]));
        }
        match crate::bridge::outbound::fetch(request).await {
            Ok(response) => {
                let status = response.status();
                let mut body = String::from_utf8_lossy(response.body()).into_owned();
                if body.len() > HTTP_GET_BODY_LIMIT {
                    body.truncate(truncation_boundary(&body, HTTP_GET_BODY_LIMIT));
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

/// Largest index `<= limit` that falls on a UTF-8 character boundary of `s`.
///
/// `String::truncate` panics on a non-boundary index, and any multibyte body
/// longer than the limit would hit one.
fn truncation_boundary(s: &str, limit: usize) -> usize {
    let mut index = limit.min(s.len());
    while !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// In-guest SSRF guard for [`TemplateServer::http_get`]: refuses URLs whose
/// scheme is not http(s) or whose host is loopback, private-range,
/// link-local, or `*.localhost`.
///
/// This is defense in depth for hosts without outbound filtering (`wasmtime
/// serve -Shttp`); on wasmCloud/Cosmonic the workload `allowedHosts` policy
/// is the authoritative control (a DNS name resolving to a private address is
/// only visible host-side). Returns the denial reason, or `None` if allowed.
/// Opt out for local development with `MCP_HTTP_GET_ALLOW_LOCAL=true`.
fn deny_private_target(uri: &http::Uri) -> Option<String> {
    match uri.scheme_str() {
        Some("http") | Some("https") => {}
        other => {
            return Some(format!(
                "denied: scheme {:?} is not allowed (use http or https)",
                other.unwrap_or("none")
            ));
        }
    }

    if std::env::var("MCP_HTTP_GET_ALLOW_LOCAL").is_ok_and(|v| v == "true" || v == "1") {
        return None;
    }

    let host = uri.host().unwrap_or_default().trim_matches(['[', ']']);
    let local = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        }
    } else {
        host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost")
    };

    local.then(|| {
        format!("denied: {host} is a local/private target (set MCP_HTTP_GET_ALLOW_LOCAL=true to permit in development)")
    })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TemplateServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                // Skills over MCP rides on the resources primitive: declaring
                // it is what makes `skill://` URIs discoverable at all.
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Template MCP server running as a WebAssembly component on \
             Cosmonic Desktop. Use `echo`, `add`, or `current_time` to \
             verify connectivity, `http_get` to test outbound HTTP, then \
             replace them with your own tools.\n\n\
             This server publishes skills — playbooks describing when and how \
             to use its tools. Read `skill://index.json` for the catalog, then \
             read `skill://<name>/SKILL.md` for any skill whose description \
             matches the task at hand.",
        )
    }

    /// Skills over MCP: every skill file, plus the catalog, as resources.
    ///
    /// The whole set is returned in one page — a server embedding enough
    /// skills for that to be unwieldy should honour `request.cursor` and set
    /// `next_cursor` on the result instead.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(skills::resources()))
    }

    /// Parameterized `skill://` URIs, so a client can construct a skill
    /// request without having enumerated every resource first.
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            skills::resource_templates(),
        ))
    }

    #[tracing::instrument(name = "resources.read", skip(self, _context), fields(uri = %request.uri))]
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (mime_type, text) = skills::read(&request.uri).ok_or_else(|| {
            ErrorData::resource_not_found(
                format!(
                    "no resource at {}; read {} for the skills this server serves",
                    request.uri,
                    skills::INDEX_URI
                ),
                None,
            )
        })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri).with_mime_type(mime_type)
        ])
        .into())
    }
}
