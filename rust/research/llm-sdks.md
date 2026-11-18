# LLM SDKs: what `@anthropic-ai/sdk` and `openai` do implicitly

Researched 2026-11-18 for the Rust provider adapters (reqwest + hand-written SSE, no SDK). Goal: list everything
the two npm SDKs do on sasacode's behalf, so the port reproduces it on purpose or drops it on purpose.

Sources and short names:
- `A/` = `@anthropic-ai/sdk` **0.128.0** `src/`, `O/` = `openai` **7.23.0** `src/`, both installed in the main
  checkout under `node_modules/.bun/@anthropic-ai+sdk@0.128.0+fff7ddf946fac095/node_modules/@anthropic-ai/sdk` and
  `node_modules/.bun/openai@7.23.0+fff7ddf946fac095/node_modules/openai` (the worktree has no `node_modules`).
  Citations point at the shipped TS `src/`; Bun runs the compiled `.mjs` built from it.
- sasacode paths are relative to the repo root (`packages/...`); a bare basename (`agent.ts`) is unique under `packages/`.
  The adapters are identical in main and this worktree.
- **Probe** = a Bun script run against the real adapters (`getProvider(api).stream`) with a capturing `fetch`
  or a local `Bun.serve`, Bun 1.4.4, macOS arm64. Probe results are marked *(probe)*.

Labels in the trap list: **reproduce** (behaviour users or tests depend on), **relax** (a TS quirk the port may fix,
decide explicitly), **drop** (SDK feature sasacode never uses).

## 0. How sasacode configures the SDKs

| Use | Options passed | Cite |
|---|---|---|
| Anthropic stream | `apiKey` only if `req.apiKey` set; `baseURL: model.baseUrl ?? "https://api.anthropic.com"`; `maxRetries: req.maxRetries ?? 8`; `defaultHeaders: model.headers`; `fetch` if `req.fetch` | `packages/ai/src/anthropic.ts:65-72` |
| Anthropic call | `client.messages.stream(params, { signal: req.signal, headers })`, `headers` = `{anthropic-beta}` or undefined | `anthropic.ts:98-105,138` |
| OpenAI chat/responses | `apiKey: req.apiKey \|\| "none"`; `baseURL: model.baseUrl ?? "https://api.openai.com/v1"`; `maxRetries: req.maxRetries ?? 8`; `defaultHeaders: model.headers`; `fetch` | `packages/ai/src/openai-chat.ts:30-39` |
| OpenAI calls | `client.chat.completions.create(body, { signal })`, `client.responses.create(body, { signal })` | `openai-chat.ts:129-139`, `openai-responses.ts:59-69` |
| openai-codex | delegates to openai-responses with `maxRetries: Math.min(req.maxRetries ?? 2, 2)`, extra headers, and a `fetch` that rewrites the body | `packages/bundled/src/openai-codex/provider.ts:54-98` |
| Model listing | `new Anthropic({apiKey?, baseURL: p.baseUrl, maxRetries: 1, defaultHeaders})` / `new OpenAI({apiKey \|\| "none", baseURL: p.baseUrl, maxRetries: 1, ...})` | `packages/cli/src/list-models.ts:60,66` |

Never passed: `timeout`, `fetchOptions`, `dangerouslyAllowBrowser`, `logLevel`, `defaultQuery`, `middleware`,
`organization`, `project`. `maxRetries` comes from config `maxRetries` (integer 0..20, default 8;
`packages/cli/src/config.ts:78,126`) through `packages/agent/src/agent.ts:568`.

## 1. Retries

Both SDKs share one design (Stainless). Differences are called out.

- **When a retry can happen: only before a 2xx response is returned.** The retry decision runs in
  `makeRequest` on a failed `fetch` or a non-OK status (`A/client.ts:1159-1281`, `O/client.ts:1652-1846`). Once a
  200 SSE response is handed to the stream parser, nothing is retried by the SDK: mid-stream drops and in-stream
  error events surface as errors (sections 4 and 7).
- **Retried statuses** (`A/client.ts:1412-1450`, `O/client.ts:1993-2014`): header `x-should-retry: true` → retry
  (any status, even 400; *probe*: a 400 with it was sent twice); `x-should-retry: false` → never; otherwise 408, 409,
  429 and every status >= 500. Anthropic only: a 401 on a request that used its OAuth token cache invalidates the
  token and retries once (`A/client.ts:1418-1428`; never hit when an API key is set).
- **Retried transport failures**: any `fetch` rejection (connection refused, DNS, reset, the SDK's own timeout
  abort) unless the caller's signal is aborted (`A/client.ts:1159-1227`, `O/client.ts:1657-1695`).
  `isTimeout` = AbortError, or `/timed? ?out/i` in the error or its cause (`A/client.ts:1169-1171`).
- **Retry budget**: default 2 in the SDKs (`A/client.ts:664`, `O/client.ts:944-946`); sasacode passes 8
  (codex: at most 2; listing: 1). `maxRetries: N` means up to N+1 requests.
- **Backoff** when no usable server hint (`A/client.ts:1492-1505`, `O/client.ts:2085-2098`):
  `delay_ms = min(0.5 * 2^n, 8.0) * (1 - rand[0,1) * 0.25) * 1000`, `n = maxRetries - retriesRemaining`
  (0 for the first retry). So 0.5, 1, 2, 4, 8, 8, 8, 8 s, each scaled by (0.75, 1]. With 8 retries the silent
  wait is 29.6-39.5 s (the "約35秒" in `packages/bundled/src/auto-reconnect.ts:8`). *Probe*: 503 retries sent at
  0, 410, 1230, 3091 ms.
- **Server hints** (`A/client.ts:1452-1488`, `O/client.ts:2016-2083`):
  1. `retry-after-ms`: `parseFloat` → ms.
  2. `retry-after`: only if (1) gave nothing. `parseFloat` → seconds × 1000, else HTTP-date `Date.parse(v) - now`.
  3. Validity differs. **Anthropic**: used if `0 < ms <= 2^31-1` (so a `retry-after: 3600` really waits an hour;
     `retry-after-ms: 0` falls through to `retry-after`/backoff because it tests `!timeoutMillis`).
     **OpenAI**: used if finite and `0 <= ms <= 60000` (0 means retry at once; above 60 s → backoff instead).
  *Probe*: `retry-after-ms: 120` → retries at 0, 122, 243 ms in both.
- **Request headers per attempt**: `X-Stainless-Retry-Count: <attempt index>` (0 first), built per attempt
  (`A/client.ts:1139-1140,1574`, `O/client.ts:1543-1545,2221`). The JSON body is re-sent unchanged; no idempotency
  key is sent (`O/client.ts:490,2209`: `idempotencyHeader` is never set on `OpenAI`; Anthropic has none).
- **Backoff sleep and abort**: Anthropic's sleep resolves early on abort and the next `makeRequest` throws
  `APIUserAbortError` (`A/internal/utils/sleep.ts:9-26`, `A/client.ts:1148-1150`); OpenAI's sleep rejects and is
  turned into `APIUserAbortError` (`O/internal/utils/sleep.ts:27-34`, `O/client.ts:2066-2079`).
- Error bodies of retried responses are cancelled unread (`A/client.ts:1244`, `O/client.ts:1796-1799`).

**Retry layers in sasacode around the SDK** (each outer attempt gets a fresh SDK budget):
1. SDK, pre-headers only (above).
2. Adapter: Anthropic re-sends up to 3 times after a 400 whose text matches `adjust()` and before any content
   (`anthropic.ts:109-128,196-202`); `withEffort` walks effort candidates on `isEffortRejected`
   (`packages/ai/src/effort.ts:24-58`).
3. Agent: a reply cut mid-stream (`stopReason "error"`) is asked again after 1, 2, 4 s, at most 3 times, then
   `throw new Error(errorMessage)` (`packages/agent/src/agent.ts:470-481`).
4. Plugin `auto-reconnect` (bundled, on by default, `packages/bundled/src/index.ts:43,67`): on `agent_end` with an
   error matching `TRANSIENT` it re-injects after 30, 60, 120, 120 s; otherwise it polls the endpoint every 5 s
   for up to 10 min (`auto-reconnect.ts:38,57-65,159-182,203-212`).

## 2. Timeouts

- Default 600 000 ms in both (`A/client.ts:1659`, `O/client.ts:2330`); sasacode never overrides it.
  Anthropic `messages.create` passes `timeout: 600000` explicitly for streams (`A/resources/messages/messages.ts:91-101`).
  The non-streaming formula (`3_600_000 * max_tokens / 128_000` ms, error if > 600 000, i.e. `max_tokens` > 21 333;
  `A/client.ts:1507-1519`) never applies: sasacode always streams.
- **The timer covers time-to-headers only.** It is armed around the `fetch` call and cleared when `fetch`
  resolves, i.e. when response headers arrive (`A/client.ts:1363-1367`, `O/client.ts:1960,1989`). OpenAI's body
  timer skips streams (`O/client.ts:1369-1376`); Anthropic has none (`A/internal/parse.ts:23-30`). **Neither SDK
  puts an idle/read timeout on the SSE body**, and sasacode has no stall watchdog (no match for stall/idle/
  watchdog in `packages/{ai,agent}/src`). A stalled stream waits until the runtime gives up or the user aborts.
- On expiry the SDK aborts its own controller; that counts as a retryable connection timeout. After the last
  attempt: `APIConnectionTimeoutError("Request timed out.")` (`A/client.ts:1219-1221`, `O/client.ts:1712-1731`).
  Worst case with 8 retries: 9 × 600 s + ≤ 39.5 s ≈ 91 min before the user sees an error (see the Bun caveat below).
- Runtime (Bun 1.4.4) limits, *probe* with a server that never answers / an SSE body that stops after one event:
  plain `fetch` (no SDK) against a local `Bun.serve({ idleTimeout: 0 })`, capped at 330 s. Both the headers stall
  and the body stall (`reader.read()` after the first event) were **still pending at 330 s**, with no error. So no
  Bun limit was observed up to 330 s; 330–600 s was not measured (the 91 min figure assumes Bun adds none there).
  A stalled SSE body hangs at least 330 s; nothing in the SDKs or sasacode ends it except an abort.
- Header `X-Stainless-Timeout: 600` (seconds, truncated) is sent by **Anthropic only**: OpenAI builds it from the
  caller's options, where sasacode set no timeout (`O/client.ts:2164-2165,2215,2222`; `A/client.ts:1575`). *Probe*
  confirms.

## 3. Headers

Merge order, later wins, case-insensitive, a later value **replaces** (no append, except `x-stainless-helper`),
`null` deletes (`A/internal/headers.ts:84,100-134`; same in `O/internal/headers.ts`):
SDK defaults → auth → `defaultHeaders` (= `model.headers`) → body headers (`content-type`) → per-request headers
(`A/client.ts:1569-1586`; `O/client.ts:2216-2235`). So `model.headers` can override `anthropic-version`,
`user-agent`, `x-api-key`/`authorization`, `content-type`; only per-request headers beat it. Because a per-request
`anthropic-beta` replaces the model's, sasacode merges them itself (`anthropic.ts:103`).

Exact wire headers *(probe, local `Bun.serve`, `model.headers` unset)*:

| Header | Anthropic `POST {base}/v1/messages` | OpenAI `POST {base}/chat/completions` and `/responses` | Source |
|---|---|---|---|
| `accept` | `application/json` (also for SSE) | `application/json` | `A/client.ts:1572`, `O/client.ts:2219` |
| `content-type` | `application/json` | `application/json` | `A/internal/request-options.ts:137-144` |
| auth | `x-api-key: <key>` | `authorization: Bearer <key>` (`"none"` if no key) | `A/client.ts:948-953`, `O/client.ts:838-842` |
| `anthropic-version` | `2023-06-01` | - | `A/client.ts:1580` |
| `anthropic-beta` | `thinking-binding-controls-2026-08-01` (+ model's), official host and binding models only | - | `anthropic.ts:98-105` |
| `user-agent` | `Anthropic/JS 0.128.0` | `OpenAI/JS 7.23.0` | `A/client.ts:967`, `O/client.ts:856-857` |
| `x-stainless-lang` / `-package-version` | `js` / `0.128.0` | `js` / `7.23.0` | `A/internal/detect-platform.ts:58-110,192-194` |
| `x-stainless-os` / `-arch` | `MacOS`/`Linux`/`Windows`… / `arm64`/`x64`… | same | idem (`normalizePlatform`, `normalizeArch`) |
| `x-stainless-runtime` / `-version` | `node` / `v26.3.0` (Bun reports itself as node) | same | idem |
| `x-stainless-retry-count` | `0`, `1`, … | same | section 1 |
| `x-stainless-timeout` | `600` | absent | section 2 |
| `x-stainless-helper-method` | `stream` | absent | `A/lib/MessageStream.ts:173` |
| `openai-organization` / `openai-project` | - | only if env `OPENAI_ORG_ID` / `OPENAI_PROJECT_ID` set | `O/client.ts:524-525,2225-2226` |
| added by Bun's fetch | `accept-encoding: gzip, deflate, br, zstd`, `connection: keep-alive`, `content-length`, `host` | same | *probe* |

Codex adds (as `model.headers`, so in the defaultHeaders layer): `chatgpt-account-id`, `originator: sasacode`,
`OpenAI-Beta: responses=experimental`, `version: <codex cli version>`, `session-id` (`openai-codex/provider.ts:54-65`).

Which headers servers need cannot be established from these sources. Contract headers: auth,
`anthropic-version`, `anthropic-beta`, `content-type`, the codex set. Telemetry: `user-agent`, `x-stainless-*`.
Unknown whether Command Code, Ollama, llama.cpp or vLLM key on any telemetry header.

Implicit environment reads (all bypassed when sasacode passes the option, used when it does not):
- Anthropic, no `req.apiKey` (no key in env var/keychain for that provider): `ANTHROPIC_API_KEY` → `x-api-key`,
  `ANTHROPIC_AUTH_TOKEN` → `Authorization: Bearer` (`A/client.ts:624-628`); with neither, the profile/OAuth chain
  in `$ANTHROPIC_CONFIG_DIR` or `$XDG_CONFIG_HOME/anthropic` or `~/.config/anthropic` (`ANTHROPIC_PROFILE`,
  `active_config`; `A/client.ts:700-722`, `A/core/credentials.ts:90-110,310-350`), which also adds an OAuth
  `anthropic-beta` (`A/client.ts:1033-1056`). Nothing found → `Error("Could not resolve authentication method. …")`
  before any request (`A/client.ts:890-917`). The env key is sent to **any** `baseURL`, e.g. Command Code.
- `ANTHROPIC_CUSTOM_HEADERS` / `OPENAI_CUSTOM_HEADERS` (newline-separated `Name: value`, below `defaultHeaders`;
  `A/client.ts:670-680`, `O/client.ts:623-633`). `ANTHROPIC_LOG` / `OPENAI_LOG` (`A/client.ts:661`, `O/client.ts:616`).
- `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`: only in model listing, where `p.baseUrl` may be undefined
  (`list-models.ts:60,66`; `A/client.ts:616`, `O/client.ts:519`); streaming always passes a base URL.
- The release binary is `bun build --compile --no-compile-autoload-dotenv` (`scripts/build.ts:1,32`), so all of
  the above come from the real process env only, never from a `.env` in the (untrusted) cwd (`scripts/build.ts:30-31`).

## 4. SSE parsing

Common to both (`A/internal/decoders/line.ts`, `A/core/streaming.ts:276-399`; `O/internal/decoders/line.ts`,
`O/core/streaming.ts:530-735`):
- Bytes are cut into events at the first blank line (`\n\n`, `\r\r`, `\r\n\r\n`), then into lines at `\r\n`, `\r`
  or `\n`. Lines are decoded with a non-fatal `TextDecoder` (invalid UTF-8 → U+FFFD).
- Line handling: strip one trailing `\r`; empty line dispatches if `event` or any `data` was seen; `:` lines are
  comments; split at the first `:`, drop one leading space of the value; `event` sets the name, `data` lines are
  joined with `\n`; `id`, `retry` and unknown fields are ignored.
- No content-type check: a 200 response is parsed as SSE whatever its type (`A/internal/parse.ts:23-30`,
  `O/internal/parse.ts:24-45`). A JSON body yields no events.

| | Anthropic `Stream.fromSSEResponse` (`A/core/streaming.ts:52-158`) | OpenAI `Stream.fromSSEResponse` (`O/core/streaming.ts:106-192`) |
|---|---|---|
| Dispatch by | `event:` name. Whitelist `message_start/_delta/_stop`, `content_block_start/_delta/_stop`, `completion`, `message`, agent/session names (`:68-133`); `ping` skipped; **anything else, including events without `event:`, is silently dropped** | `data` only. Every event not named `thread.*` is JSON-parsed and yielded; the name is ignored except `error` (`:138-158`) |
| `[DONE]` | not special | `data: [DONE]` ends the stream; later bytes are ignored (`:133-136`) |
| Bad JSON | `JSON.parse` throws `SyntaxError` | `SyntaxError("Error reading response: malformed server-sent event JSON.")` plus two `console.error` lines (`:140-147`). An empty `data:` (e.g. `event: ping` + `data:`) is bad JSON *(probe)* |
| Error in stream | `event: error` → `APIError(status undefined, body = parsed data or raw text, type = body.error.type)` (`:139-143`) | `event: error` → `APIError(undefined, data.error ?? data)` (`:149-151`); **any** event whose `data.error` is truthy → `APIError(undefined, data.error)` (`:153-155`) |
| EOF without blank line | last event lost | flushed once (`:582-588`) |
| Abort | AbortError while reading ends iteration quietly (`:146-149`) | same, and the reader is raced against the abort signal (`:402-480`, `:172-180`) |

Anthropic adds a second layer, `MessageStream`, which sasacode iterates (`anthropic.ts:138-139`):
- It accumulates a snapshot and throws `AnthropicError` on order violations: `Unexpected event order, got X before
  "message_start"`; a second `message_start` before `message_stop`; `request ended without sending any chunks`
  when the stream ends without `message_start` (`A/lib/MessageStream.ts:522-529,566-579`). *Probe*: a data-only
  stream (no `event:` lines) ends with "request ended without sending any chunks".
- On `content_block_stop` of a `tool_use` it parses the buffered JSON with a partial-JSON parser
  (`A/lib/MessageStream.ts:673-683`, `A/internal/message-stream-utils.ts:10-29`, `A/_vendor/partial-json-parser/parser.ts:278`).
  Truncated JSON (`{"a":"b`, `{"a":1,`) passes and reaches sasacode's `parseToolInput` as `rawInput`; **malformed
  JSON (`{"a":1}}`, `not json`) throws and kills the stream** with `AnthropicError("JSON Parse error: …")` *(probe)*.
  OpenAI paths never parse tool arguments in the SDK.
- Every non-SDK error (e.g. a socket error from the body) is re-wrapped as `AnthropicError(error.message)` with
  `cause` (`A/lib/MessageStream.ts:379-398`), so `.code` is lost; the message survives.
- Its async iterator rejects only readers already waiting; an error with no reader waiting marks it done, and if
  queued events remain, iteration ends quietly instead of throwing (`A/lib/MessageStream.ts:688-749`). Timing-dependent;
  the probe (error in the same chunk as deltas) threw in 3/3 runs.
- Unknown `delta.type` values are passed through (`checkNever` is a no-op, `A/internal/utils/values.ts:117`).

## 5. URL construction and query

- Both: `new URL(baseURL + path)`, dropping one `/` when `baseURL` ends with `/` and `path` starts with `/`
  (`A/client.ts:979-995`, `O/client.ts:921-941`). No other normalisation: no `/v1` detection, no double-slash
  cleanup beyond that one, a query string inside `baseURL` breaks the URL (path is appended after it).
- Paths: Anthropic `/v1/messages` (`A/resources/messages/messages.ts:99`) — so the base must **not** end in `/v1`
  (Command Code: `https://api.commandcode.ai/provider` → `/provider/v1/messages`; *probe* with a trailing `/`
  gives the same URL). OpenAI `/chat/completions`, `/responses` (`O/resources/chat/completions/completions.ts:195-201`,
  `O/resources/responses/responses.ts:229-235`) — the base **must** include `/v1` (`packages/host/src/models.ts:4-28`).
- No query parameters on the streaming calls. Listing: Anthropic `GET {base}/v1/models?limit=100`, then
  `after_id=<last_id>` while `has_more` (`A/resources/models.ts:53-66`, `A/core/pagination.ts:150-196`); OpenAI
  `GET {base}/models`, one page only (`O/core/pagination.ts:138-139`).

## 6. Request body serialization

- `JSON.stringify(body)` (`A/internal/request-options.ts:137-144`, `O/internal/request-options.ts:115-122`): keys in
  insertion order, `undefined` members dropped, `null` kept, `NaN`/`Infinity` → `null`, integers without `.0`,
  lone surrogates written as `\udXXX`. No request compression; Bun sets `content-length`.
- Key order *(probe)*: Anthropic `model, max_tokens, system, messages, tools, stream, cache_control[, thinking,
  output_config][, sampling…, extraBody…]` (`anthropic.ts:85-96`; `tools: []` is sent even when empty; `stream`
  keeps its slot when `MessageStream` re-adds it). OpenAI chat `model, messages, stream, stream_options,
  max_tokens|max_completion_tokens[, tools][, sampling…][, chat_template_kwargs][, reasoning_effort|reasoning]`
  (`openai-chat.ts:79-137`). Responses `model, instructions, input, stream, store, max_output_tokens[, tools]
  [, sampling…][, reasoning, include]` (`openai-responses.ts:28-66`). `Object.assign` onto an existing key keeps
  its original position (extraBody overriding `max_tokens` stays second).
- Explicit `null` matters: chat assistant `content: null` with tool calls (`openai-chat.ts:283`).
- Anthropic `messages.create` strips `user_profile_id` and `workspace_id` from the body (`A/resources/messages/messages.ts:73`) and may
  `console.warn` for deprecated models (`A/resources/messages/messages.ts:74-89`).
- Codex rewrites the serialized body: drops `max_output_tokens, temperature, top_p, metadata, user`, sets
  `store: false` (keeps slot), appends `prompt_cache_key` (`openai-codex/provider.ts:50,68-79`).

## 7. Errors and what the user sees

Message format (identical code in both, `A/core/error.ts:40-59`, `O/core/error.ts:37-56`):
`msg = error.message` if present (string as is, else JSON) / else `JSON.stringify(error)` / else raw text; then
`"<status> <msg>"`, `"<status> status code (no body)"`, `"<msg>"`, or `"(no status code or body)"`.
The `error` passed in differs:
- **Anthropic**: the whole parsed body (`A/client.ts:1266-1280`), whose top level has no `message`, so the user
  sees the full compact JSON: `401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}`.
- **OpenAI**: `body.error` (`O/client.ts:864-872`, `O/core/error.ts:68`); a body without `error` is wrapped first.
  `{"error":{"message":"Rate limit reached",…}}` → `429 Rate limit reached`; `{"error":"model 'x' not found"}` →
  `404 "model 'x' not found"` (quotes kept); `{"detail":"Not Found"}` → `404 {"detail":"Not Found"}`.
  `.code`, `.param`, `.type` come from that object (`O/core/error.ts:31-34`); `code` may be a number (vLLM `400`).
- Non-JSON body: `500 Internal Server Error`; empty body: `502 status code (no body)` (both, *probe*).
- In-stream errors have no status: Anthropic `{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}`;
  OpenAI `reasoning_effort none is not supported` (from `data.error.message`) *(probe)*.
- Transport: `APIConnectionError("Connection error.")` with the real error in `cause`
  (`ECONNREFUSED: Unable to connect…`, *probe*); timeout `Request timed out.`; abort `Request was aborted.`
  (`A/core/error.ts:110-129`, `O/core/error.ts:106-125`).
- Classes by status: 400 BadRequest, 401 Authentication, 403 PermissionDenied, 404 NotFound, 409 Conflict,
  422 UnprocessableEntity, 429 RateLimit, >=500 InternalServer, else APIError (`A/core/error.ts:61-107`).

**Every place sasacode inspects SDK errors** (the Rust error type must carry status, code, type, param and this
exact message text):

| Site | Test | Rust must provide |
|---|---|---|
| `anthropic.ts:192`, `openai-chat.ts:224`, `openai-responses.ts:179` | abort class or `req.signal.aborted` → `stopReason "aborted"` | an abort kind |
| `anthropic.ts:194-195` | 400 and `/prompt is too long\|context window/i` on the message → `ContextOverflowError` | `400 {json}` text |
| `anthropic.ts:109-128,196-202` | 400, no content yet, message matches `thinking\.type\.disabled\|disabled.*not supported`, `effort` (not `messages[.\[]\|signature\|reasoning_content`), `block_binding`, `bound to a different conversation` | same text |
| `openai-chat.ts:41-48` (also used by responses) | `code === "context_length_exceeded"` or `type === "exceed_context_size_error"` or `/context length\|context window\|context size\|too many tokens\|maximum context/i` | code/type/message, also for in-stream errors |
| `effort.ts:24-36` | status 400/422, or no status and (`String(code) === "400"` or `/BadRequest\|invalid_request/i` on type); `${param} ${message}` matched against effort regexes | status optional, numeric or string code |
| `openai-responses.ts:72-73` | first event `type: "error"` (data-only, so the SDK yielded it) → `APIError(undefined, event, event.message)` | same classification for both error shapes |
| `openai-responses.ts:169-175` | `response.failed` / `error` events → `ContextOverflowError` if `/context/i`, else `Error(message)` | |
| `types.ts:58-63` `droppedMidReply` | `/ECONNRESET\|EPIPE\|ETIMEDOUT\|ConnectionClosed\|socket connection was closed\|socket hang up\|^terminated$/i` on `${code} ${message}` → cut reply, not an error | a body-read-error kind (reqwest never produces these strings) |
| `openai-codex/provider.ts:99-102` | `.status` 401/403 → `ChatGPT rejected the login (401): 401 …. Run: sasacode login` | status on the error |
| `auto-reconnect.ts:38` `TRANSIENT` | `/^(?:408\|409\|425\|429\|5\d\d)\b\|overloaded\|temporarily unavailable\|rate.?limit/i` on the final message | message starting with `<status> ` |
| `agent.ts:368-372,423-425` | `ContextOverflowError` → `context_limit` hook; anything else → `error = e.message` → `agent_end` | |
| `packages/bundled/src/diagnostics.ts:53-93` | wraps the SDK's `fetch`, so it sees every retry as an attempt; checks `content-type` includes `text/event-stream` | hook inside the retry loop |

Tests that pin this text: `packages/ai/test/adapters.test.ts:72,210-213,313-316`,
`packages/ai/test/provider-contracts.test.ts:108,148,161-183`, `packages/bundled/test/openai-codex.test.ts:144`,
`packages/bundled/test/auto-reconnect.test.ts:48-110`, `packages/e2e/test/cli.e2e.test.ts:83,114-119`.

User-visible path: `agent.ts:424` → TUI `エラー: {e.message}` (`packages/tui/src/app.ts:541-542`, control chars
stripped by `plain`, `packages/tui/src/views.ts:18-20`) or headless stderr `error: {e.message}`, exit 1
(`packages/cli/src/headless.ts:71`). With default `maxRetries` 8:

| Case | SDK | User sees | auto-reconnect |
|---|---|---|---|
| 401 (Anthropic) | no retry | `エラー: 401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}` | not TRANSIENT; endpoint online → nothing |
| 401 (OpenAI) | no retry | `エラー: 401 Incorrect API key provided: …` | same |
| 429 | 8 retries honouring `retry-after` (Anthropic unbounded, OpenAI ≤ 60 s) | `エラー: 429 {…}` (Anthropic) / `エラー: 429 Rate limit reached…` | TRANSIENT → notice, retry after 30/60/120/120 s |
| 500 text / 502 empty | 8 retries, ~30-40 s | `エラー: 500 Internal Server Error` / `エラー: 502 status code (no body)` | TRANSIENT |
| connection refused | 8 retries, ~30-40 s, no feedback | `エラー: Connection error.` (cause dropped) | offline → polls every 5 s up to 10 min |
| no headers for 600 s | 9 attempts | after ≈ 91 min (Bun limit 330–600 s unmeasured) `エラー: Request timed out.` | not TRANSIENT |
| Anthropic in-stream `overloaded_error` | none | `エラー: {"type":"error","error":{"type":"overloaded_error",…}}` | TRANSIENT (`overloaded`) |
| socket drop mid-stream | none | headless `the connection dropped mid-reply; asking again` (`headless.ts:65-68`); 4th cut in a row → `エラー: the stream ended before the reply was complete` | not TRANSIENT |

## 8. Abort / cancel

`req.signal` comes from a per-request controller that the run's controller forwards to
(`agent.ts:552-555`); a `stream_delta` plugin can abort it alone (reported as `stopped`, `agent.ts:603-605`).

| Phase | Anthropic | OpenAI |
|---|---|---|
| already aborted | `APIUserAbortError` before fetch (`A/client.ts:1148-1150`, `A/lib/MessageStream.ts:205`) | same (`O/client.ts:1637-1639`) |
| waiting for headers | fetch rejects → `APIUserAbortError`, not retried (`A/client.ts:1162-1164`) | same (`O/client.ts:1659-1661`) |
| backoff sleep | sleep resolves, next attempt throws `APIUserAbortError` | sleep rejects → `APIUserAbortError` |
| mid-stream | stream ends quietly, then `MessageStream` throws `APIUserAbortError` (`A/lib/MessageStream.ts:218-220`) → `stopReason "aborted"` | **stream ends quietly, no error** (`O/core/streaming.ts:172-180,402-480`) → adapter sees a normal end → `markIfCut` → `stopReason "error"`, `errorMessage` = INCOMPLETE_STREAM *(probe)* |

Consequence of the OpenAI row: `respond()` skips the user-abort cleanup (`agent.ts:483-489` keys on `"aborted"`),
so half-streamed tool calls stay in the message and get "not run" results (`packages/agent/src/tool-exec.ts:158-188`);
the run still ends with cause `aborted` (`agent.ts:393-397`). No test pins this.

## Traps for the Rust port

- [ ] **reproduce** Retry only before a 2xx: statuses 408/409/429/>=500 and transport errors; obey `x-should-retry`
      true/false first; `maxRetries` from config (default 8), codex `min(n, 2)`, listing 1.
- [ ] **reproduce** Backoff `min(0.5·2^n, 8) s × (1 − U[0,0.25))`; hints `retry-after-ms`, then `retry-after`
      (seconds or HTTP-date). **Decide** one cap: Anthropic SDK accepts up to 2^31−1 ms, OpenAI only ≤ 60 s.
- [ ] **reproduce** Send `x-stainless-retry-count` per attempt only if keeping Stainless headers (decide below).
- [ ] **reproduce** A 600 s **time-to-headers** timeout, retried like a connection error. **Do not** use
      reqwest `.timeout()` on streaming requests: it is a whole-request deadline and would kill long replies.
- [ ] **relax** Add an SSE idle timeout or not: neither SDK nor sasacode has one today (section 2, Bun result).
- [ ] **reproduce** Header merge order defaults → auth → `model.headers` → `content-type` → per-request, replacing
      case-insensitively; per-request `anthropic-beta` must be merged with the model's by hand (`anthropic.ts:103`).
- [ ] **reproduce** `anthropic-version: 2023-06-01`, `x-api-key` (Anthropic), `Authorization: Bearer <key|none>`
      (OpenAI, `"none"` when keyless), `content-type: application/json`, `accept: application/json`.
- [ ] **decide** Keep `user-agent: Anthropic/JS 0.128.0` / `OpenAI/JS 7.23.0` and the `x-stainless-*` set (some
      gateways may sniff them; unknown) or send a sasacode UA.
- [ ] **decide** `accept-encoding`: Bun advertises `gzip, deflate, br, zstd` and decodes transparently; reqwest
      sends nothing unless the `gzip`/`brotli`/`zstd`/`deflate` features are on.
- [ ] **relax** Anthropic SDK env fallbacks (`ANTHROPIC_API_KEY` sent to any base URL, `ANTHROPIC_AUTH_TOKEN`,
      profile/OAuth chain), `*_CUSTOM_HEADERS`, `OPENAI_ORG_ID`/`OPENAI_PROJECT_ID`, `*_BASE_URL` in listing only.
      Decide each; keyless Anthropic today fails with "Could not resolve authentication method…".
      Never load a `.env` from the cwd (the release binary disables it, `scripts/build.ts:30-32`).
- [ ] **reproduce** URL = base + path with one-slash dedupe; Anthropic appends `/v1/messages`, OpenAI appends
      `/chat/completions` / `/responses` to a base that already has `/v1`. Listing: Anthropic paginates
      `limit=100` + `after_id`, OpenAI one page.
- [ ] **reproduce** Body = `JSON.stringify` semantics: insertion order (serde_json `preserve_order`), omitted vs
      explicit `null`, JS number format (`1` not `1.0`), `tools: []` for Anthropic, extraBody overrides keep slot.
- [ ] **reproduce** SSE lines end at `\r\n`, `\r` or `\n`; multi-line `data` joined with `\n`; one leading space
      stripped; comments ignored; decode UTF-8 lossily (`from_utf8_lossy`, never fail on bad bytes).
- [ ] **reproduce** Anthropic: dispatch by `event:` name, skip `ping`, ignore unknown names and data-only
      events; `event: error` → error without status, message = compact JSON of the data.
- [ ] **relax** Anthropic `MessageStream` strictness: order violations and "request ended without sending any
      chunks" are errors today; malformed tool JSON kills the stream today (the port can hand it to
      `parseToolInput` as `rawInput` instead).
- [ ] **reproduce** OpenAI: ignore event names except `error`; stop at `data: [DONE]`; any event with truthy
      `error` → error from `data.error`; flush an unterminated last event at EOF.
- [ ] **relax** OpenAI throws on any non-JSON `data` (even empty `data:` after `event: ping`) and logs to stderr;
      the port can skip empty data.
- [ ] **reproduce** JSON parsing must accept what JS accepts: lone surrogate escapes (`"\ud83d"`) parse in JS but
      fail in serde_json by default — needs a lenient string path or the stream dies.
- [ ] **reproduce** Error text: `"<status> <msg>"` with Anthropic = whole body JSON, OpenAI = `body.error`
      (`.message` or JSON, string errors keep quotes), `"<status> status code (no body)"`, raw text bodies.
      `TRANSIENT`, `isContextOverflow`, `adjust`, `isEffortRejected` and the codex 401 text all parse it.
- [ ] **reproduce** Error fields: `status: Option<u16>`, `code` (string **or number**), `type`, `param`; in-stream
      errors have no status. Transport errors read `Connection error.` / `Request timed out.` /
      `Request was aborted.`.
- [ ] **reproduce** Replace `droppedMidReply`'s Bun/Node string match with an error kind: a body read error
      after headers = cut reply (INCOMPLETE_STREAM path), not a thrown error.
- [ ] **relax** Abort mid-stream yields `"aborted"` for every API; TS gives OpenAI-style adapters
      `"error"` + INCOMPLETE_STREAM and keeps half tool calls (section 8).
- [ ] **reproduce** Abort is checked before the request, while waiting for headers and during backoff sleep
      (no further attempt after abort).
- [ ] **reproduce** The diagnostics hook observes every HTTP attempt including retries (wrap inside the retry
      loop), and the codex body rewrite applies to every attempt's serialized body.
- [ ] **drop** Stainless extras sasacode never uses: middleware, OAuth token cache, x509/workload identity,
      data residency, `dangerouslyAllowBrowser`, idempotency keys, deprecated-model warnings.
