# Tool reference

Progressive disclosure: this file is a supporting resource of the
`mcp-server-template` skill. Clients pull it only when the SKILL.md body is not
enough. It is reachable at `skill://mcp-server-template/references/TOOLS.md`,
which is also where the relative link in SKILL.md resolves to.

## `echo`

```json
{"message": "string"}
```

Returns the message unchanged as a single text content block. No length limit
beyond the transport's request-body cap.

## `add`

```json
{"a": 1.5, "b": 2.5}
```

Both arguments are IEEE-754 doubles. Returns:

```json
{"structuredContent": {"sum": 4.0}, "content": [{"type": "text", "text": "{\"sum\":4.0}"}]}
```

Non-finite results are not produced by any in-range input pair, but a client
should still treat `sum` as a JSON number and not assume integrality.

## `current_time`

No arguments. Returns unix epoch milliseconds as a decimal string. The clock is
the host's; a Wasm component has no independent time source.

## `http_get`

```json
{"url": "https://example.com/path"}
```

Behavior and limits:

| Condition | Result |
|---|---|
| Success | Text `HTTP <status>\n<body>`, body truncated to 4 KiB with `…[truncated]` |
| Scheme not `http`/`https` | `isError: true`, text `denied: scheme …` |
| Loopback / private / link-local / `*.localhost` host | `isError: true`, text `denied: <host> is a local/private target …` |
| Host absent from the workload `allowedHosts` | `isError: true`, text `fetch failed: … HttpRequestDenied` |
| Upstream does not respond within the deadline | `isError: true`, text `fetch failed: … timed out` |
| Unparseable URL | JSON-RPC error `-32602` |

Truncation is UTF-8 safe: the cut lands on a character boundary, so the text is
always valid even when the upstream body is multibyte.

The outbound deadline (`MCP_OUTBOUND_TIMEOUT_MS`, default 30000) and the
buffered-response cap (`MCP_OUTBOUND_MAX_BYTES`, default 4 MiB) are set by the
deployment, not by the caller.
