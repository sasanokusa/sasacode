// `sasacode login`: sign in with a ChatGPT plan the way OpenAI's Codex CLI does — PKCE (S256),
// Codex's public client, and a callback on localhost:1455 (the redirect registered for that
// client, so no other port works). When the browser cannot reach this machine (SSH) or the port
// is taken, the URL the browser ends up on can be pasted instead.
import { execFile } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { createServer, type Server } from "node:http";
import { createInterface } from "node:readline/promises";
import {
  CODEX_AUTHORIZE_URL,
  CODEX_CLIENT_ID,
  CODEX_SCOPES,
  CODEX_TOKEN_URL,
  clearCodexAuth,
  codexAuthPath,
  planType,
  readCodexAuth,
  tokensFrom,
  writeCodexAuth,
} from "./auth.ts";

const PORT = 1455;
const REDIRECT_URI = `http://localhost:${PORT}/auth/callback`;

export interface LoginDeps {
  fetch?: typeof fetch;
  /** Asks the user for the pasted URL or code; aborted once the browser has come back. */
  prompt?: (question: string, signal: AbortSignal) => Promise<string>;
  out?: (line: string) => void;
  openBrowser?: (url: string) => void;
  /** Where credentials go (tests). */
  path?: string;
  /** Skip the callback server (tests). */
  noServer?: boolean;
}

export function authorizeUrl(challenge: string, state: string): string {
  const url = new URL(CODEX_AUTHORIZE_URL);
  for (const [k, v] of Object.entries({
    response_type: "code",
    client_id: CODEX_CLIENT_ID,
    redirect_uri: REDIRECT_URI,
    scope: CODEX_SCOPES,
    code_challenge: challenge,
    code_challenge_method: "S256",
    state,
    id_token_add_organizations: "true",
    codex_cli_simplified_flow: "true",
    originator: "sasacode",
  }))
    url.searchParams.set(k, v);
  return url.toString();
}

/** A bare code, or the whole URL the browser landed on (then its state must match). */
export function parseCallback(input: string, state: string): string | undefined {
  const text = input.trim();
  if (!text) return undefined;
  if (!/^https?:\/\//.test(text)) return text.split("#")[0] || undefined;
  const url = new URL(text);
  const got = url.searchParams.get("state");
  if (got && got !== state) throw new Error("login failed: the pasted URL belongs to another login attempt (state mismatch)");
  return url.searchParams.get("code") ?? undefined;
}

/** Resolves with the code when the browser comes back; undefined when the port is taken. */
function callbackServer(state: string): Promise<{ code: Promise<string>; server: Server } | undefined> {
  return new Promise((resolve) => {
    let deliver!: (code: string) => void;
    let fail!: (e: Error) => void;
    const code = new Promise<string>((res, rej) => ((deliver = res), (fail = rej)));
    code.catch(() => {});
    const server = createServer((req, res) => {
      const url = new URL(req.url ?? "/", REDIRECT_URI);
      if (url.pathname !== "/auth/callback") return void res.writeHead(404).end();
      const error = url.searchParams.get("error");
      const got = url.searchParams.get("code");
      const page = (status: number, text: string) =>
        res.writeHead(status, { "content-type": "text/html; charset=utf-8" }).end(`<p style="font:16px system-ui;margin:3em">${text}</p>`);
      if (error) {
        page(400, "sasacode: ログインできませんでした。ターミナルを確認してください。");
        return fail(new Error(`login failed: ${error} ${url.searchParams.get("error_description") ?? ""}`.trim()));
      }
      if (!got || url.searchParams.get("state") !== state) return page(400, "sasacode: このログインの応答ではありません。");
      page(200, "sasacode: ログインしました。このタブは閉じてかまいません。");
      deliver(got);
    });
    server.once("error", () => resolve(undefined));
    server.listen(PORT, "127.0.0.1", () => resolve({ code, server }));
  });
}

async function ask(question: string, signal: AbortSignal): Promise<string> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  try {
    return await rl.question(question, { signal });
  } finally {
    rl.close();
  }
}

export async function codexLogin(deps: LoginDeps = {}): Promise<void> {
  const out = deps.out ?? ((s: string) => console.log(s));
  const verifier = randomBytes(32).toString("base64url");
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  const state = randomBytes(16).toString("hex");
  const url = authorizeUrl(challenge, state);

  const cb = deps.noServer ? undefined : await callbackServer(state);
  out("Sign in with your ChatGPT account in the browser:");
  out(`  ${url}`);
  (deps.openBrowser ?? openInBrowser)(url);
  let code: string | undefined;
  const stopAsking = new AbortController();
  try {
    if (cb) {
      out(`Waiting for the browser (${REDIRECT_URI}).`);
      out("If it cannot reach this machine (e.g. over SSH), paste the URL it ends up on here and press enter.");
      // Whichever comes first: the callback, or a pasted URL. An empty line keeps waiting.
      const pasted = (async () => {
        const answer = await (deps.prompt ?? ask)("> ", stopAsking.signal);
        return parseCallback(answer, state);
      })();
      pasted.catch(() => {});
      code = await Promise.race([cb.code, pasted.then((c) => c ?? cb.code)]);
    } else {
      out(`Port ${PORT} is in use, so the browser cannot come back here: after signing in, paste the URL it ends up on.`);
      code = parseCallback(await (deps.prompt ?? ask)("> ", stopAsking.signal), state);
    }
  } finally {
    stopAsking.abort();
    cb?.server.close();
  }
  if (!code) throw new Error("login cancelled: no authorization code");

  const res = await (deps.fetch ?? fetch)(CODEX_TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "authorization_code", client_id: CODEX_CLIENT_ID, code, code_verifier: verifier, redirect_uri: REDIRECT_URI }),
  });
  if (!res.ok) throw new Error(`login failed: the token exchange returned ${res.status}: ${(await res.text()).slice(0, 200)}`);
  const tokens = tokensFrom((await res.json()) as Record<string, string>);
  const path = deps.path ?? codexAuthPath();
  writeCodexAuth(tokens, path);
  const plan = planType(tokens.idToken);
  out(`Signed in${plan ? ` (ChatGPT ${plan})` : ""}. Credentials: ${path}`);
  out("Use it with: sasacode -m openai-codex/gpt-5.5   (or pick one in /model)");
}

function openInBrowser(url: string): void {
  const cmd = process.platform === "darwin" ? "open" : process.platform === "win32" ? "explorer" : "xdg-open";
  execFile(cmd, [url], () => {});
}

export function codexLogout(path = codexAuthPath()): string {
  return clearCodexAuth(path) ? "Signed out (local credentials removed)." : "Not signed in.";
}

export function codexStatus(path = codexAuthPath()): { ok: boolean; text: string } {
  const t = readCodexAuth(path);
  if (!t) return { ok: false, text: "Not signed in to a ChatGPT plan. Run: sasacode login" };
  const plan = planType(t.idToken);
  const mins = t.expiresAt ? Math.round((t.expiresAt - Date.now()) / 60_000) : undefined;
  return {
    ok: true,
    text: `Signed in${plan ? ` (ChatGPT ${plan})` : ""}.${mins !== undefined ? ` Access token: ${mins > 0 ? `${mins} min left` : "expired"} (refreshed automatically).` : ""}`,
  };
}
