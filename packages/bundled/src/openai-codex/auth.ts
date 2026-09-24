// ChatGPT-plan credentials for the openai-codex provider: where they are stored and how they are
// kept fresh. The login itself (browser, callback server) is in login.ts.
import { mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

/** The public OAuth client of OpenAI's Codex CLI. There is no secret; PKCE protects the flow. */
export const CODEX_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann";
export const CODEX_AUTHORIZE_URL = "https://auth.openai.com/oauth/authorize";
export const CODEX_TOKEN_URL = "https://auth.openai.com/oauth/token";
export const CODEX_SCOPES = "openid profile email offline_access";
const AUTH_CLAIM = "https://api.openai.com/auth";
/** Refresh when less than this is left on the access token. */
const REFRESH_WINDOW_MS = 5 * 60 * 1000;

export interface CodexTokens {
  access: string;
  refresh: string;
  idToken?: string;
  accountId?: string;
  /** Epoch ms, from the access token's `exp`. */
  expiresAt?: number;
}

/** Stored beside the user's config, never in a project: a repository must not supply a login. */
export function codexAuthPath(home = process.env.SASACODE_HOME ?? join(homedir(), ".sasacode")): string {
  return join(home, "codex-auth.json");
}

export function readCodexAuth(path = codexAuthPath()): CodexTokens | undefined {
  try {
    const t = JSON.parse(readFileSync(path, "utf8")) as Partial<CodexTokens>;
    if (!t.access || !t.refresh) return undefined;
    return { ...t, access: t.access, refresh: t.refresh, accountId: t.accountId ?? accountId(t.idToken) ?? accountId(t.access), expiresAt: expiry(t.access) };
  } catch {
    return undefined;
  }
}

export function writeCodexAuth(tokens: CodexTokens, path = codexAuthPath()): void {
  mkdirSync(dirname(path), { recursive: true });
  // Temp file + rename: a crash mid-write must not leave a broken credential behind.
  const tmp = `${path}.tmp`;
  writeFileSync(tmp, `${JSON.stringify(tokens, null, 2)}\n`, { mode: 0o600 });
  renameSync(tmp, path);
  cached = undefined;
}

export function clearCodexAuth(path = codexAuthPath()): boolean {
  const had = !!readCodexAuth(path);
  rmSync(path, { force: true });
  cached = undefined;
  return had;
}

// ── token claims (read, never verified: the server does that) ───────────────

export function jwtClaims(token: string | undefined): Record<string, unknown> | undefined {
  try {
    return JSON.parse(Buffer.from(token!.split(".")[1]!, "base64url").toString("utf8"));
  } catch {
    return undefined;
  }
}

const authClaim = (token: string | undefined) => jwtClaims(token)?.[AUTH_CLAIM] as Record<string, unknown> | undefined;
const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

export function accountId(token: string | undefined): string | undefined {
  return str(authClaim(token)?.chatgpt_account_id);
}

export function planType(idToken: string | undefined): string | undefined {
  return str(authClaim(idToken)?.chatgpt_plan_type);
}

function expiry(token: string): number | undefined {
  const exp = jwtClaims(token)?.exp;
  return typeof exp === "number" ? exp * 1000 : undefined;
}

/** Turn a token endpoint response into stored credentials. */
export function tokensFrom(data: { access_token?: string; refresh_token?: string; id_token?: string }, previous?: CodexTokens): CodexTokens {
  if (!data.access_token) throw new Error("the token response has no access_token");
  const refresh = data.refresh_token ?? previous?.refresh;
  if (!refresh) throw new Error("the token response has no refresh_token");
  const idToken = data.id_token ?? previous?.idToken;
  return {
    access: data.access_token,
    refresh,
    idToken,
    accountId: accountId(idToken) ?? accountId(data.access_token) ?? previous?.accountId,
    expiresAt: expiry(data.access_token),
  };
}

// ── refresh ─────────────────────────────────────────────────────────────────

export async function refreshCodexTokens(tokens: CodexTokens, path = codexAuthPath(), doFetch: typeof fetch = fetch): Promise<CodexTokens> {
  const res = await doFetch(CODEX_TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: tokens.refresh, client_id: CODEX_CLIENT_ID }),
  });
  if (!res.ok) throw new Error(`ChatGPT login refresh failed (${res.status}): ${(await res.text().catch(() => "")).slice(0, 200)}`);
  const next = tokensFrom((await res.json()) as Record<string, string>, tokens);
  writeCodexAuth(next, path);
  return next;
}

/**
 * Credentials to use now, refreshed first when about to expire; undefined when nobody signed in.
 * A failed refresh keeps a token that still works rather than breaking the session.
 */
export async function currentCodexTokens(path = codexAuthPath(), doFetch: typeof fetch = fetch, now = Date.now()): Promise<CodexTokens | undefined> {
  const stored = readCodexAuth(path);
  if (!stored) return undefined;
  if (stored.expiresAt !== undefined && stored.expiresAt - now > REFRESH_WINDOW_MS) return stored;
  try {
    return await refreshCodexTokens(stored, path, doFetch);
  } catch (e) {
    if (stored.expiresAt !== undefined && stored.expiresAt > now) return stored;
    throw e;
  }
}

/** The same, shared for a few seconds: one request asks for the token more than once. */
let cached: { at: number; tokens: Promise<CodexTokens | undefined> } | undefined;
export function codexTokens(): Promise<CodexTokens | undefined> {
  if (!cached || Date.now() - cached.at > 10_000) {
    const tokens = currentCodexTokens().catch((e) => {
      cached = undefined;
      throw e;
    });
    cached = { at: Date.now(), tokens };
  }
  return cached.tokens;
}
