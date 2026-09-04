---
name: mcp-server-template
description: Operate the mcp-server-template MCP server — verify connectivity with echo/add/current_time, fetch a URL with http_get under the outbound allow-list, and interpret this server's error shapes. Use when connected to this server and deciding which of its tools to call or why a call failed.
---

# Using the mcp-server-template MCP server

<!--
THIS FILE IS SERVED TO CLIENTS over MCP as `skill://mcp-server-template/SKILL.md`
(Skills over MCP, `io.modelcontextprotocol/skills`). It is embedded into the
component at compile time by `src/skills.rs`.

The skill's URI name is the package name from Cargo.toml, so this directory
never needs renaming. When you fork this template:
  1. rewrite the frontmatter `name` (match Cargo.toml) + `description` — the
     description is the trigger text clients match against, so say WHEN to use
     the server, not just what it is,
  2. replace the body with your server's operating knowledge, and
  3. add any supporting files to `references/` and list them in the `SKILLS`
     table in `src/skills.rs`.
Delete this comment block when you do.
-->

This server runs as a sandboxed WebAssembly component. It is **stateless**:
every request is self-contained, nothing you set on one call carries into the
next, and there is no session to resume.

## When to use it

This is a template deployment. Use it to prove an MCP client can reach the
server, to check that outbound network policy is wired up, and as the worked
example when building a real server from the same template.

## Tools

| Tool | Arguments | Returns |
|---|---|---|
| `echo` | `message: string` | The same string. Round-trip check. |
| `add` | `a: number`, `b: number` | `structuredContent: {"sum": number}` plus a text fallback. |
| `current_time` | none | Current UTC time as unix epoch milliseconds, as text. |
| `http_get` | `url: string` | `HTTP <status>` followed by up to 4 KiB of the body. |

Full argument schemas, limits, and failure modes: [references/TOOLS.md](references/TOOLS.md).

## How to work with this server

1. **Prefer `add` over `echo` when checking the client's structured-output
   path** — `echo` only exercises text content, `add` exercises
   `structuredContent`, which is where client bugs usually surface.
2. **`http_get` is governed by two independent policies.** The workload's
   `allowedHosts` allow-list is enforced by the host and is **deny-all by
   default**; separately, the server refuses loopback, private-range,
   link-local, and `*.localhost` targets in-guest. A URL can be rejected by
   either, and the messages differ — read the text of the error rather than
   assuming the URL was malformed.
3. **Do not retry a denial.** `denied: …` and `HttpRequestDenied` are policy
   decisions, not transient failures; retrying produces the same result. Tell
   the user which host needs to be added to `allowedHosts` instead.
4. **Do retry once on `fetch failed: … timed out`** — that is an upstream
   deadline (30 s by default), and a second attempt is reasonable before
   reporting the upstream as unreachable.
5. **Bodies are truncated, not paginated.** `http_get` returns at most 4 KiB
   and marks the cut with `…[truncated]`. There is no offset argument; if you
   need more of a document, ask the user for a more specific URL.

## Reading errors

- `"isError": true` inside a `result` — the tool ran and failed. The text is
  written for you; surface its content to the user.
- JSON-RPC `error` with code `-32602` — the request itself was malformed
  (missing or ill-typed `params`). Fix the call, do not report an outage.
- HTTP `403 Forbidden` before any JSON-RPC response — the server's
  DNS-rebinding guard rejected the `Host` header. The deployment's
  `MCP_ALLOWED_HOSTS` does not list the name you connected under.
- HTTP `413` — the request body exceeded the transport limit.

## Server metadata without a protocol handshake

`GET /` on this server returns a JSON discovery document: server name and
version, MCP spec version, endpoint paths, tool names, and the skills it
serves. It is the cheapest way to confirm a deployment is live and to see what
it offers before opening an MCP session.
