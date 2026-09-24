import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { StreamEvent } from "@sasacode/ai";
import {
  accountId,
  type CodexTokens,
  clearCodexAuth,
  codexAuthPath,
  currentCodexTokens,
  planType,
  readCodexAuth,
  writeCodexAuth,
} from "../src/openai-codex/auth.ts";
import { authorizeUrl, codexLogin, parseCallback } from "../src/openai-codex/login.ts";
import { codexProvider, FALLBACK_MODELS } from "../src/openai-codex/provider.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-codex-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));
let n = 0;
const file = () => join(dir, `auth-${++n}.json`);

/** Unsigned JWTs with just the claims we read (the server verifies them, we do not). */
const jwt = (claims: Record<string, unknown>) => ["x", Buffer.from(JSON.stringify(claims)).toString("base64url"), "x"].join(".");
const AUTH = "https://api.openai.com/auth";
const access = (secs: number, account = "acct-1") => jwt({ exp: Math.floor(Date.now() / 1000) + secs, [AUTH]: { chatgpt_account_id: account } });
const tokens = (secs = 3600): CodexTokens => ({
  access: access(secs),
  refresh: "rt-old",
  idToken: jwt({ [AUTH]: { chatgpt_account_id: "acct-1", chatgpt_plan_type: "pro" } }),
  accountId: "acct-1",
});
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

// ── credentials ─────────────────────────────────────────────────────────────

test("credentials live in the sasacode home, readable only by the user", () => {
  expect(codexAuthPath("/h")).toBe("/h/codex-auth.json");
  const path = file();
  writeCodexAuth(tokens(), path);
  expect(statSync(path).mode & 0o777).toBe(0o600);
  expect(readCodexAuth(path)).toMatchObject({ refresh: "rt-old", accountId: "acct-1" });
  expect(planType(readCodexAuth(path)!.idToken)).toBe("pro");
  expect(clearCodexAuth(path)).toBe(true);
  expect(readCodexAuth(path)).toBeUndefined();
  expect(clearCodexAuth(path)).toBe(false);
});

test("the account id is recovered from the tokens, and junk is not fatal", () => {
  const path = file();
  writeFileSync(path, JSON.stringify({ access: access(3600, "acct-from-access"), refresh: "r" }));
  expect(readCodexAuth(path)?.accountId).toBe("acct-from-access");
  expect(accountId("not a jwt")).toBeUndefined();
  writeFileSync(path, "{ torn");
  expect(readCodexAuth(path)).toBeUndefined();
});

test("a token with time left is used as is; one about to expire is refreshed (form-encoded) and saved", async () => {
  const path = file();
  writeCodexAuth(tokens(3600), path);
  let calls = 0;
  const noFetch = (async () => (calls++, json({}))) as unknown as typeof fetch;
  expect((await currentCodexTokens(path, noFetch))?.refresh).toBe("rt-old");
  expect(calls).toBe(0);

  writeCodexAuth(tokens(60), path); // inside the 5 minute window
  let sent: { type?: string | null; body?: string } = {};
  const refresh = (async (_url: string, init: RequestInit) => {
    sent = { type: new Headers(init.headers).get("content-type"), body: String(init.body) };
    return json({ access_token: access(3600), refresh_token: "rt-new" });
  }) as unknown as typeof fetch;
  const next = await currentCodexTokens(path, refresh);
  expect(sent.type).toBe("application/x-www-form-urlencoded");
  expect(new URLSearchParams(sent.body).get("grant_type")).toBe("refresh_token");
  expect(next?.refresh).toBe("rt-new");
  expect(readCodexAuth(path)?.refresh).toBe("rt-new");
});

test("a failed refresh keeps a token that still works, and fails once it has expired", async () => {
  const path = file();
  const down = (async () => json({ error: "server" }, 500)) as unknown as typeof fetch;
  writeCodexAuth(tokens(60), path);
  expect((await currentCodexTokens(path, down))?.refresh).toBe("rt-old");
  writeCodexAuth(tokens(-60), path);
  await expect(currentCodexTokens(path, down)).rejects.toThrow(/refresh failed \(500\)/);
});

// ── provider ────────────────────────────────────────────────────────────────

const model = { id: "gpt-5.5", provider: "openai-codex", api: "openai-codex", baseUrl: "https://chatgpt.com/backend-api/codex", contextWindow: 400_000, maxOutput: 64_000 };

/** Answers with a scripted Responses stream and keeps what was sent. */
function sse(events: unknown[], seen: { url?: string; headers?: Headers; body?: any }): typeof fetch {
  return (async (url: string, init: RequestInit) => {
    seen.url = String(url);
    seen.headers = new Headers(init.headers);
    seen.body = JSON.parse(String(init.body));
    const body = events.map((e) => `event: ${(e as { type: string }).type}\ndata: ${JSON.stringify(e)}\n\n`).join("");
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  }) as unknown as typeof fetch;
}

const reply = [
  { type: "response.output_item.added", item: { type: "message", id: "m1" } },
  { type: "response.output_text.delta", item_id: "m1", delta: "Hi" },
  { type: "response.output_item.done", item: { type: "message", id: "m1" } },
  { type: "response.completed", response: { usage: { input_tokens: 100, output_tokens: 7, input_tokens_details: { cached_tokens: 40 } } } },
];

async function run(provider: ReturnType<typeof codexProvider>, extra: Record<string, unknown> = {}) {
  const out: StreamEvent[] = [];
  const req = { model, system: "be brief", messages: [{ role: "user" as const, content: [{ type: "text" as const, text: "hello" }], timestamp: 0 }], tools: [], maxTokens: 1000, sampling: { temperature: 0.2 }, ...extra };
  for await (const e of provider.stream(req)) out.push(e);
  return out;
}

test("requests go to the ChatGPT backend with the plan's token and headers, minus the fields it rejects", async () => {
  const seen: { url?: string; headers?: Headers; body?: any } = {};
  const t = tokens();
  const provider = codexProvider(() => "sess-1", { tokens: async () => t, fetch: sse(reply, seen) });
  const events = await run(provider, { tools: [{ name: "read", description: "", parameters: { type: "object" } }] });
  expect(seen.url).toBe(`${model.baseUrl}/responses`);
  expect(seen.headers?.get("authorization")).toBe(`Bearer ${t.access}`);
  expect(seen.headers?.get("chatgpt-account-id")).toBe("acct-1");
  expect(seen.headers?.get("openai-beta")).toBe("responses=experimental");
  expect(seen.headers?.get("session-id")).toBe("sess-1");
  expect(seen.headers?.get("version")).toBeTruthy();
  expect(seen.body).toMatchObject({ model: "gpt-5.5", instructions: "be brief", store: false, stream: true, prompt_cache_key: "sess-1" });
  expect(seen.body.max_output_tokens).toBeUndefined();
  expect(seen.body.temperature).toBeUndefined();
  expect(seen.body.tools[0]).toMatchObject({ type: "function", name: "read" });
  const done = events.at(-1) as Extract<StreamEvent, { type: "done" }>;
  expect(done.message).toMatchObject({ api: "openai-codex", content: [{ type: "text", text: "Hi" }], usage: { input: 60, cacheRead: 40, output: 7 } });
});

test("not signed in, or signed out on the server: the user is told to run sasacode login", async () => {
  await expect(run(codexProvider(() => "s", { tokens: async () => undefined }))).rejects.toThrow(/sasacode login/);
  const rejected = (async () => json({ error: { message: "token expired" } }, 401)) as unknown as typeof fetch;
  await expect(run(codexProvider(() => "s", { tokens: async () => tokens(), fetch: rejected }), { maxRetries: 0 })).rejects.toThrow(/rejected the login \(401\).*sasacode login/);
});

test("models come from the backend when signed in; none are offered when signed out", async () => {
  const listing = (async (url: string) =>
    String(url).includes("/models?client_version=")
      ? json({ models: [{ slug: "gpt-5.6-sol", context_window: 1_050_000 }, { slug: "codex-auto-review", visibility: "hide" }] })
      : json({}, 500)) as unknown as typeof fetch;
  const signedIn = codexProvider(() => "s", { tokens: async () => tokens(), fetch: listing });
  expect(await signedIn.listModels!(model as never, undefined)).toEqual([{ id: "gpt-5.6-sol", contextWindow: 1_050_000 }]);
  const down = codexProvider(() => "s", { tokens: async () => tokens(), fetch: (async () => json({}, 503)) as unknown as typeof fetch });
  expect((await down.listModels!(model as never, undefined)).map((m) => m.id)).toEqual(FALLBACK_MODELS);
  expect(await codexProvider(() => "s", { tokens: async () => undefined }).listModels!(model as never, undefined)).toEqual([]);
});

// ── login ───────────────────────────────────────────────────────────────────

test("the sign-in URL uses PKCE and the redirect registered for the Codex client", () => {
  const url = new URL(authorizeUrl("challenge", "state-1"));
  expect(url.searchParams.get("redirect_uri")).toBe("http://localhost:1455/auth/callback");
  expect(url.searchParams.get("code_challenge_method")).toBe("S256");
  expect(url.searchParams.get("scope")).toBe("openid profile email offline_access");
});

test("a pasted code or redirect URL is accepted; one from another attempt is not", () => {
  expect(parseCallback("abc#xyz", "s")).toBe("abc");
  expect(parseCallback("http://localhost:1455/auth/callback?code=c1&state=s", "s")).toBe("c1");
  expect(() => parseCallback("http://localhost:1455/auth/callback?code=c1&state=other", "s")).toThrow(/state mismatch/);
  expect(parseCallback("  ", "s")).toBeUndefined();
});

test("login exchanges the code with its PKCE verifier and stores the tokens", async () => {
  const path = file();
  let form = new URLSearchParams();
  let challenge = "";
  const exchange = (async (_url: string, init: RequestInit) => {
    form = new URLSearchParams(String(init.body));
    return json({ access_token: access(3600, "acct-login"), refresh_token: "rt-login", id_token: jwt({ [AUTH]: { chatgpt_plan_type: "plus" } }) });
  }) as unknown as typeof fetch;
  const lines: string[] = [];
  await codexLogin({
    noServer: true,
    path,
    fetch: exchange,
    openBrowser: (u) => (challenge = new URL(u).searchParams.get("code_challenge")!),
    prompt: async () => "the-code",
    out: (l) => lines.push(l),
  });
  expect(form.get("grant_type")).toBe("authorization_code");
  expect(form.get("code")).toBe("the-code");
  // The verifier sent now hashes to the challenge sent to the browser.
  const { createHash } = await import("node:crypto");
  expect(createHash("sha256").update(form.get("code_verifier")!).digest("base64url")).toBe(challenge);
  expect(readCodexAuth(path)).toMatchObject({ refresh: "rt-login", accountId: "acct-login" });
  expect(lines.join("\n")).toContain("ChatGPT plus");
});

test("an empty paste cancels, and a failed exchange says why", async () => {
  const base = { noServer: true, path: file(), openBrowser: () => {}, out: () => {} };
  await expect(codexLogin({ ...base, prompt: async () => "" })).rejects.toThrow(/cancelled/);
  const refused = (async () => new Response("bad code", { status: 400 })) as unknown as typeof fetch;
  await expect(codexLogin({ ...base, prompt: async () => "c", fetch: refused })).rejects.toThrow(/returned 400/);
});
