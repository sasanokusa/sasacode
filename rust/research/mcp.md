# MCP client: what the TS does, rmcp, and the choice for the port

Researched 2026-11-18 for `crates/sasacode-mcp` (ADR-010 points here).

Sources and short names used below:
- `index.ts` = `packages/mcp/src/index.ts`; tests in `packages/mcp/test/`.
- `SDK/` = `@modelcontextprotocol/sdk` **1.30.1** (`dist/esm/`), installed in the main checkout at
  `node_modules/.bun/@modelcontextprotocol+sdk@1.30.1/node_modules/@modelcontextprotocol/sdk` (the worktree
  has no `node_modules`).
- `rmcp/` = crate `rmcp` **3.10.0** (newest on crates.io per `cargo search rmcp`, released 2026-11-12,
  `rmcp/CHANGELOG.md:10`), read from `~/.cargo/registry/src/index.crates.io-*/rmcp-3.10.0/`. Same tree:
  https://github.com/modelcontextprotocol/rust-sdk/tree/rmcp-v3.10.0/crates/rmcp, docs https://docs.rs/rmcp/3.10.0.

## 1. What sasacode's MCP client does

Users: `createMcpPlugin` loaded from `config.mcpServers` (`packages/cli/src/setup.ts:268-272`; line 271 also
merges plugin manifests' `mcpServers`, a source that goes away with ADR-002) and
`connectServers` reused by the bundled browsr plugin with `toolNames: "plain"`, `alwaysLoad: true`
(`packages/bundled/src/browsr.ts:71`). No other bundled plugin uses MCP (web-search and auto-reconnect do not).

| Aspect | Behaviour | Cite |
|---|---|---|
| Config | `{command,args?,env?,cwd?}` or `{url,headers?}`, plus `disabled`, `alwaysLoad`, `timeout` (ms), `toolNames: "prefixed"\|"plain"`. The CLI only checks `mcpServers` is an object of objects, so `timeout`/`toolNames` reach the client although `config.ts`'s type omits them | `index.ts:7-18`; `packages/cli/src/config.ts:47-49,83` |
| Transports | stdio (`StdioClientTransport`) when `command` is present, else Streamable HTTP. **No legacy SSE fallback**: `SSEClientTransport` is never imported | `index.ts:1-3,173-182` |
| `${VAR}` | Only the `${NAME}` form (`[A-Za-z_][A-Za-z0-9_]*`), unset → `""`. Applied to env **values**, header **values** and the **url**; not to command, args, cwd or names | `index.ts:45-48,178,182` |
| stdio spawn | `cross-spawn`, `shell: false`, `windowsHide` on win32, stdio `[pipe,pipe,pipe]`, `cwd: cfg.cwd ?? api.cwd` | `index.ts:175-181`; `SDK/client/stdio.js:65-75` |
| stdio env | **Not the full environment**: whitelist `HOME LOGNAME PATH SHELL TERM USER` (win32: `APPDATA HOMEDRIVE HOMEPATH LOCALAPPDATA PATH PROCESSOR_ARCHITECTURE SYSTEMDRIVE SYSTEMROOT TEMP USERNAME USERPROFILE PROGRAMFILES`), values starting `()` skipped, then expanded `cfg.env` on top | `index.ts:178`; `SDK/client/stdio.js:8-42` |
| stderr | Piped, control chars `[\x00-\x08\x0b-\x1f\x7f-\x9f]` removed (keeps `\t`,`\n`; drops `\r`, C1), last 2000 UTF-16 units kept, never reset across reconnects; last line appended to a start error as ` — <line>` | `index.ts:183-185,222` |
| Framing | NDJSON; trailing `\r` stripped; an unparsable line is reported via `onerror` and skipped; buffer > 10 MiB without a newline closes the transport | `SDK/shared/stdio.js:2-35`; `SDK/client/stdio.js:90-99,129-141` |
| Handshake | `initialize` with `protocolVersion: "2025-11-25"`, `capabilities: {}`, `clientInfo: {name:"sasacode", version: api.version}`; accepts server versions `2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05, 2024-10-07`, else throws; then `notifications/initialized`. Default 60 s timeout (no options passed) | `index.ts:170,198`; `SDK/client/index.js:100-107,274-317`; `SDK/types.js:2-4,8` |
| HTTP | POST `accept: application/json, text/event-stream`; JSON or SSE replies; `mcp-session-id` and `mcp-protocol-version` headers; user headers spread **last** (override everything); 202 for notifications; after `initialized` opens a GET SSE stream (405 tolerated); SSE resumption 2 retries, 1 s ×1.5, max 30 s. No DELETE on close (`terminateSession` never called), no 404 re-init | `SDK/client/streamableHttp.js:7-12,59-77,281-288,297-415,433-449` |
| tools/list | Only if server capability `tools` (#101). Paged; stops on a repeated cursor or after 100 pages (#49); `while (cursor)` ends on `""` too | `index.ts:231-252` |
| Tool registration | Name `mcp__<server>__<tool>` with `[^a-zA-Z0-9_-]`→`_`, cut to 64; `plain` = raw tool name. description `description ?? title ?? name`; `parameters = inputSchema`; `kind: "other"` (always asks); `concurrent = annotations.readOnlyHint === true`; tools no longer listed are unregistered | `index.ts:51-53,253-283` |
| tools/call | Uses the client of the moment (survives reconnects); not connected → error `MCP server X is not connected (<error or status>).`; `timeout: cfg.timeout ?? 600_000`, `signal` from ctx; thrown errors → `MCP <server>/<tool> failed: <msg>` | `index.ts:267-280` |
| Result mapping | text→text; image→image; audio→`[audio content omitted]`; resource→its text, else `[resource <uri> (<mimeType or binary>)]`; resource_link→`[resource link <name or ""> <uri>]`; `structuredContent` (JSON) only if no content, then legacy `toolResult`; nothing → `(no content)`; `isError` passed through (absent stays absent) | `index.ts:336-359` |
| Truncation | Text total (UTF-16 units) > 100 000 → texts joined with `\n` (images add empty lines), saved by `saveOutput("mcp-<server>-<tool>" with [^\w.-]→_)` to `$SASACODE_HOME or ~/.sasacode` `/tmp/<prefix>-<ms>-<6 base36>.log` (dir 0700, file 0600, files > 7 days pruned once per process); returns first 100 000 + `\n[Output truncated at 100000 of <total> characters. Full output saved to <file>]`, images kept after | `index.ts:20,348-358`; `packages/plugin-api/src/index.ts:397-435` |
| Prompts | Only if capability `prompts`; paged as above; failure → warning, tools kept. Each prompt → command `toolName(server, prompt)` (prefixed even in plain mode), hint = arg names. Args: tokens `(?:[^\s"]+\|"[^"]*")+`; `^(\w+)=(.*)$` sets a key, other words fill args in order; quotes stripped at ends. Result: text and resource-text messages joined `\n\n`, `session.inject(text, "now")` | `index.ts:214,286-321` |
| list_changed | Handler installed only if `capabilities.tools.listChanged`; re-runs tools/list sync; failure → warning | `index.ts:202-213` |
| Server→client | SDK answers `ping` with `{}`; other requests (sampling, roots, elicitation) → `-32601 Method not found`; `notifications/cancelled` and progress handled by the SDK | `SDK/shared/protocol.js:26-35,285-297` |
| Cancellation | Abort → `notifications/cancelled {requestId, reason}`, rejects with McpError -32001 (RequestTimeout, also used for aborts); responses after abort ignored | `SDK/shared/protocol.js:670-711`; `SDK/types.js:162` |
| Progress | `resetTimeoutOnProgress: true` is **dead**: a `progressToken` is sent only when `onprogress` is given (it is not), and `_onprogress` returns before resetting without a handler. Real behaviour: hard 600 s (`docs/dependencies.md` claims the opposite) | `index.ts:275`; `SDK/shared/protocol.js:424-447,643-651` |
| Output schema | `callTool` validates `structuredContent` with Ajv against `outputSchema` cached by `listTools`, throwing McpError: no `structuredContent` (and not `isError`) → -32600, mismatch → -32602 (→ `... failed: MCP error <code>: ...`); a task-required tool also throws | `SDK/client/index.js:479-510,554-558`; `SDK/types.js:165,167` |
| Errors | McpError message is `MCP error <code>: <message>` | `SDK/types.js:2029-2035` |
| Reconnect | On stdio close of the current, connected client: notice `MCP server X disconnected`, status `closed`, error `server exited`. Failed connect: client closed (no orphan, #101), error notice only on the first failure. Retry delay `min(gap·2^attempts, 30 s)`, gap = `plugins.settings.mcp.reconnectDelayMs ?? 2000` (2,4,8,16,30 s), max 5, then `gave up reconnecting` notice. `attempts` reset only after tools and prompts registered. Timers unref'd. `/mcp reconnect [name]` resets and replaces the client; stale `onclose` of a replaced client ignored | `index.ts:21-24,60-101,186-197,215-227` |
| HTTP drop | `StreamableHTTPClientTransport` fires `onclose` only from `close()`, so **an HTTP server going away never triggers the reconnect loop**; calls just fail | `SDK/client/streamableHttp.js:281-288` |
| Status / command | `MCP <connected>/<total>` status; `/mcp` lists `●/○ name: status (error) · N tools`; ja/en messages | `index.ts:119-166` |
| Shutdown | `session_end` with reason ≠ `switch` (also no reason) closes all; stdio close = end stdin, wait 2 s, SIGTERM, wait 2 s, SIGKILL | `index.ts:103-114`; `SDK/client/stdio.js:143-178` |
| Startup | `api.ready(open(...))`: headless waits (`host.settle()`) for the first attempt | `index.ts:100`; `packages/cli/src/headless.ts:85-86` |

## 2. rmcp 3.10.0

| Question | Answer | Cite |
|---|---|---|
| MSRV, edition | Rust 1.88, edition 2024 (same as the workspace) | `rmcp/Cargo.toml:13-14` |
| Runtime | tokio required (`sync, macros, rt, time`); child process adds `tokio/process` | `rmcp/Cargo.toml.orig:54,191-195` |
| Features for us | `default-features = false`, `client`, `transport-child-process`, `transport-streamable-http-client-reqwest`, `reqwest-tls-no-provider` (matches the workspace's rustls-no-provider + ring). Optional `which-command` | `rmcp/Cargo.toml.orig:138-200`; `rmcp/README.md` feature tables |
| Child stdio | Yes: `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()` returns the stderr handle. Default stderr is **inherit**; env is whatever the `tokio::process::Command` has (full parent env by default) | `rmcp/src/transport/child_process.rs:80-88,145-190` |
| Child shutdown | close stdin, wait 3 s, SIGTERM (unix), wait 3 s, kill. Dropping without `close` spawns an immediate kill on the tokio runtime | `child_process.rs:10,45-56,93-130` |
| Windows | `which_command` (feature `which-command`) because tokio "does not reliably" find `.cmd` shims; no `CREATE_NO_WINDOW` by default | `child_process.rs:224-246` |
| Streamable HTTP + headers | Yes: `custom_headers: HashMap<HeaderName,HeaderValue>`, `auth_header` (bearer). Headers named `accept`, `mcp-session-id`, `last-event-id` are **rejected**: every request built with them fails with `ReservedHeaderConflict` | `rmcp/src/transport/streamable_http_client.rs:2105-2146,2179-2210`; `transport/common/http_header.rs:16-44`; `transport/common/reqwest/streamable_http_client.rs:34-44` |
| HTTP defaults | SSE reconnect unlimited, 1 s doubling, no cap; 16 concurrent POSTs; re-initialize on 404; DELETE session on close; an ordinary POST's error goes back to that request's caller and the worker loop continues (no close event either) | `streamable_http_client.rs:1733-1817,1877-1890,2232-2247`; `transport/common/client_side_sse.rs:219-248` |
| Legacy SSE transport | None in 3.10.0 (no feature for it) | `rmcp/Cargo.toml.orig` features |
| Protocol versions | Knows 2024-11-05 … 2025-11-25 and **2026-07-28**, which drops `initialize` (SEP-2567). `ProtocolVersion::default()` = LATEST = 2026-07-28, and `InitializeRequestParams::default()` uses it, so the default `serve()` sends `initialize` with `2026-07-28`; a 1.30-era server answers `2025-11-25` and rmcp accepts any version without checking a list | `rmcp/src/model.rs:156-201,1250-1257,1604-1612`; `service/client.rs:954-1007`; `tests/test_protocol_version_negotiation.rs:169-172` |
| clientInfo | Default `Implementation::from_build_env()` = `rmcp` / `3.10.0`: must be set | `model.rs:1721-1730` |
| Lifecycle modes | `Initialize` (default for `serve`), `Discover`, `Auto` (probe `server/discover`, 10 s, fall back) | `service/client.rs:707-722,758-772` |
| Handshake timeout | None in the client: `expect_response` loops until a reply (the 60 s `init_timeout` in rmcp is the HTTP **server's** session setting); wrap `serve()` in `tokio::time::timeout` | `service/client.rs:183-270`; `transport/streamable_http_server/session/local.rs:1247-1265` |
| Request timeouts | Per request via `PeerRequestOptions {timeout, reset_timeout_on_progress, max_total_timeout}`; default **none**. High-level `call_tool`/`list_tools` take no options; use `send_request_with_option` and match `ServerResult` | `rmcp/src/service.rs:575-600,791-810,865-884`; `service/client.rs:2170-2178` |
| Cancellation | Dropping a high-level call's future sends `notifications/cancelled`; timeouts send it too | `service/client.rs:1456-1470`; `service.rs:617-630` |
| Pagination | `list_tools(Some(cursor))`; `list_all_tools`/`list_all_prompts` loop while `next_cursor.is_some()`: **no repeat/100-page guard**, `Some("")` keeps going | `service/client.rs:1947-1983` |
| Response cache | On by default (SEP-2549), `serve_stale_on_error: true`: a failed `tools/list` may return the old list; invalidated by `list_changed`; `peer.set_response_cache_config` turns it off | `service/client/cache.rs:54-64,370`; `service/client.rs:1888-1935` |
| Prompts | `list_prompts`, `get_prompt` (drives MRTR rounds) | `service/client.rs:1722,2219` |
| list_changed | `ClientHandler::on_tool_list_changed` called whatever the server's capability | `rmcp/src/handler/client.rs:255-269` |
| Server→client | Default handler: `ping` ok, `create_message` -32601, but `list_roots` answers an **empty list** (TS: -32601) | `handler/client.rs:90-111` |
| Results | `ServerResult` is `#[serde(untagged)]`: the type comes from the shape, not the request. A malformed `tools/call` result falls to `CustomResult` → `ServiceError::UnexpectedResponse`; `CallToolResult` needs one of content/structuredContent/isError (TS accepts `{}`). `isError` is just `Option<bool>` in the result, never an `Err`. No client-side outputSchema validation | `model.rs:4139-4220,5008-5034`; `service/client.rs:1475-1493` |
| Framing | 16 MiB max line, an oversize line is dropped (TS: connection closed); invalid JSON skipped; valid JSON of the wrong shape gets an Invalid Request reply | `rmcp/src/transport/async_rw.rs:58,283-332` |
| Errors / ids | `ServiceError::{McpError(ErrorData), TransportSend, TransportClosed, UnexpectedResponse, Cancelled, Timeout}`; `ErrorData` displays `<code>: <message>(<data>)`, so `to_string()` = `Mcp error: -32603: ...`. Ids: `AtomicU32RequestIdProvider` numbers; string-numeric response ids accepted | `rmcp/src/service.rs:79-97`; `rmcp/src/error.rs:4-12`; `service/client.rs:840`; `CHANGELOG.md:357` (#1021) |
| Drop detection | `peer.is_transport_closed()` or a task on `RunningService::waiting()` | `service.rs:1078,1124` |
| Churn | 1.8.0 (2026-06-22) → 2.0.0 (06-27) → 3.0.0 (07-28), then a minor about weekly to 3.10.0 | `CHANGELOG.md:10-437` |
| Cost (measured) | Over the workspace's deps: **+18 crates** (sha2 0.11/digest 0.11 stack beside 0.10, process-wrap, nix, windows 0.62 family, sse-stream, tokio-stream, tracing-attributes). rmcp is 58.5 k lines of source and the **slowest crate in a release build: 16.7 s** (tokio 11.7 s) on this Mac | scratch `cargo tree` diff and `cargo build --release --timings`, 2026-11-18 |

## 3. Recommendation: hand-write the client; rmcp only in tests

sasacode needs a small client: `initialize`, `tools/list`, `tools/call`, `prompts/list`, `prompts/get`, the
`tools/list_changed` notification, `ping` and cancel, over two transports. A hand-written
`crates/sasacode-mcp` (JSON-RPC core with an id→oneshot map, NDJSON over child stdio, Streamable HTTP on the
workspace reqwest with the SSE parser ADR-010 already hand-writes for the model APIs, then the plugin glue
ported line by line from `index.ts`) can reproduce section 1 exactly. That includes the 60 s/600 s timeouts,
the env whitelist, `MCP error <code>: <message>`, no DELETE and no HTTP reconnects, at no dependency cost.

rmcp is the official SDK and works, but for this port nearly every default would need overriding (protocol
version, clientInfo, env, stderr, timeouts, page guard, cache, headers, SSE retry policy). Its untagged result
typing and its 2026-07-28 surface (discover, MRTR, tasks, subscriptions, skills) are behaviour we don't want.
It would also be the slowest crate in the build, and it has had two semver-major releases in five weeks.

The one real cost of hand-writing: protocol **2026-07-28 removes `initialize`** (rmcp speaks it; TS SDK 1.30.1
does not, `SDK/types.js:2-4`), so parity holds today. Switch to rmcp (pinned `=3.x`, features as above) if one
of these happens: servers users need become modern-only, OAuth for remote servers is wanted (rmcp `auth`), or
sampling/elicitation/roots are to be offered.

Tests: use rmcp **server** as a dev-only peer (an implementation independent of ours). While `packages/` still
exists, also run the Rust client once against `bun packages/mcp/test/fixture-server.ts`, skipped when bun is
missing. That is the closest oracle to ADR-001.

## 4. The TS test servers and Rust equivalents

- `packages/mcp/test/fixture-server.ts` (stdio via `bun`, HTTP via `makeServer()`): capabilities
  `tools.listChanged` + `prompts`. Tools `add` (readOnlyHint, returns the sum), `fail` (`isError`), `big`
  (150 000 `x`), `toggle` (flips `late`, sends `tools/list_changed` before replying), `late`. Prompts on two
  pages (`review`, `greet` / cursor `page2` → `later`); `prompts/get` echoes the arguments. Env switches:
  `FIXTURE_NO_TOOLS` (prompts only), `FIXTURE_TOOLS_FAIL` (tools/list throws), `FIXTURE_ENDLESS_PROMPTS`
  (cursor `again` forever), `FIXTURE_LOG` (`start/exit <pid>` lines), `FIXTURE_SLOW_EXIT` (ms before exit after
  stdin ends) (`fixture-server.ts:16-81`).
- HTTP in `mcp.test.ts:13-31`: `Bun.serve` + `WebStandardStreamableHTTPServerTransport`, one transport per
  session; a request without a known session and without `authorization: Bearer secret-token` gets 401.
- `packages/bundled/test/fake-browsr.ts`: `manifest` prints a manifest v1 (`FAKE_SCHEMA` changes
  tool_schema_version); `serve` is a tools-only server with `search`/`open`. It runs via `#!` or, on Windows, a
  generated `.cmd` wrapper (`packages/bundled/test/bundled.test.ts:461-476`).

Rust plan:
- `crates/sasacode-testing` (dev-only, already has axum): `src/bin/mcp-fixture.rs` and `src/bin/fake-browsr.rs`
  on rmcp with `default-features = false, features = ["server", "transport-io",
  "transport-streamable-http-server"]`. Measured: +21 crates over the workspace + axum, including schemars,
  rand 0.10, sha2 0.11 (scratch `cargo tree`); never reaches the shipped binary. Handler: implement
  `ServerHandler::{get_info, list_tools, call_tool, list_prompts, get_prompt}`; `toggle` calls
  `context.peer.notify_tool_list_changed()`; stdio = `serve(stdio())` then `waiting()`, then sleep
  `FIXTURE_SLOW_EXIT`, log, exit (`rmcp/src/handler/server.rs:477-600`; `rmcp/src/transport/io.rs:20`;
  `rmcp/src/service/server.rs:957`).
- HTTP: a library function in sasacode-testing that mounts `StreamableHttpService` + `LocalSessionManager`
  in axum behind a middleware giving 401 for session-less requests without the bearer. Any crate can call it
  in-process (`rmcp/src/transport/streamable_http_server/tower.rs:336-352,1473`). The default
  `allowed_hosts` is localhost-only, which is fine for tests.
- Locating the binaries: `CARGO_BIN_EXE_<name>` is set only for the package's own binaries. Verified in a
  scratch workspace: `None` from another package's test even with a dev-dependency, `Some(path)` inside the
  same package (https://doc.rust-lang.org/cargo/reference/environment-variables.html). So put the
  stdio-spawning MCP and browsr tests in `crates/sasacode-testing/tests/` (dev-depend on sasacode-mcp,
  -bundled, -agent), or give sasacode-testing a helper that runs `$CARGO build -p sasacode-testing --bins` once
  and returns `target/<profile>/<name>`.
- A hand-written fixture (~200 lines, tokio + serde_json) would avoid rmcp entirely. It is not recommended:
  it shares the client author's misreadings of the spec.

## Traps for the Rust port

- [ ] Send `protocolVersion: "2025-11-25"` and accept only the five TS versions; with rmcp, never rely on
      `ProtocolVersion::default()` (2026-07-28).
- [ ] `clientInfo` is `{name: "sasacode", version: <app version>}`; capabilities `{}`.
- [ ] Default 60 s timeout for initialize, list and get-prompt; tools/call `timeout ?? 600_000` ms, a **hard**
      limit (don't "fix" the dead progress reset unless you mean to change behaviour, then update
      `docs/dependencies.md`).
- [ ] stdio env = whitelist (unix and Windows lists differ) + expanded `cfg.env`: `env_clear()` first. Note
      Windows still searches the parent PATH after `env_clear`
      (https://doc.rust-lang.org/std/process/struct.Command.html).
- [ ] No shell. On Windows resolve `npx` → `npx.cmd` via `which` (workspace dep) and set `CREATE_NO_WINDOW`:
      std finds only `.exe` without an extension; take care with `.cmd` argument quoting (same std page).
- [ ] Pipe stderr and **drain it continuously** (a full pipe blocks the server); strip control chars per
      char (C1 = U+0080–U+009F, not bytes), keep the last 2000; don't reset across reconnects.
- [ ] `cwd: cfg.cwd ?? api.cwd`; a relative `cfg.cwd` resolves against the process cwd as in Node.
- [ ] `${VAR}` only in env values, header values and url; `$VAR` untouched; unset → empty.
- [ ] HTTP: user headers override all (TS spreads them last); rmcp rejects `accept`/`mcp-session-id`/
      `last-event-id`. No DELETE on close, no 404 re-init, GET stream with 2 retries (1 s ×1.5, ≤ 30 s), 405 ok.
- [ ] An HTTP server going down must **not** start the reconnect loop (TS never fires onclose for HTTP); only
      stdio exits and failed connects do.
- [ ] Reconnect: `min(gap·2^n, 30 s)`, gap default 2000, 5 tries, one start-failure notice, give-up notice,
      reset only after tools *and* prompts are registered; ignore close events from replaced clients (#101);
      a failed attempt must close its process (6 starts → 6 exits in the #101 test). With rmcp a failed
      `serve()` drops the child = immediate kill without the stdin-EOF exit path.
- [ ] Graceful close timings: TS 2 s/2 s, rmcp 3 s/3 s; `session_end` with reason `switch` keeps servers.
- [ ] Gate `tools/list` on `capabilities.tools` and prompts on `capabilities.prompts`; list_changed handler only
      with `tools.listChanged`.
- [ ] Page loop: stop on repeated cursor and after 100 pages, treat `""` as the end (rmcp `list_all_*` loops
      forever on `FIXTURE_ENDLESS_PROMPTS`).
- [ ] With rmcp: disable the response cache, or a failed re-list returns stale tools instead of the warning.
- [ ] Unregister tools that vanished; `plain` names unsanitized; prefixed names: JS regex without `u` turns an
      astral char into **two** `_` (UTF-16 halves), then cut at 64.
- [ ] `concurrent` only when `readOnlyHint === true`; `kind` always `other`.
- [ ] Result mapping strings exactly as in section 1; `isError` absent stays absent; `{}` result is
      `(no content)` (rmcp would call `{}` an `EmptyResult` → `UnexpectedResponse`).
- [ ] Truncation counts and slices **UTF-16 units** (`encode_utf16`), must not split a surrogate pair; the
      joined text has empty lines where images were; file name/dir/modes/7-day prune as in `saveOutput`.
- [ ] Decide on Ajv outputSchema validation and task-required tools (TS throws; rmcp does neither). Record the
      choice in `rust/MAP.md`.
- [ ] Error text to the model: `MCP <server>/<tool> failed: MCP error <code>: <message>`; rmcp's own Display
      differs (`Mcp error: <code>: <message>`).
- [ ] Server requests: `ping` → `{}`, everything else `-32601` (rmcp's default `list_roots` returns `[]`).
- [ ] Cancel on abort sends `notifications/cancelled` and drops a late response.
- [ ] Framing: skip unparsable lines; TS closes on > 10 MiB without newline (rmcp drops the line, 16 MiB).
- [ ] Prompt args: JS `\w` is ASCII (`[A-Za-z0-9_]`), Rust regex `\w` is Unicode, so use `(?-u:\w)`;
      `\s` covers U+3000 in both; strip a leading and a trailing `"` independently.
- [ ] Prompt commands are always `mcp__<server>__<prompt>` (even with `toolNames: "plain"`); join text/resource
      texts with `\n\n` and inject `now`.
- [ ] Headless startup waits for the first attempt (`api.ready`); browsr additionally polls ≤ 20 s for its tools.
- [ ] Never hold a mutex across the awaits of connect or call (ADR-010); reconnect timers must not keep the
      process alive (TS `unref`).
