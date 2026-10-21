/**
 * auto-reconnect — ネットワーク障害で接続が切れたとき、復旧を検知して自動で接続を再試行する。
 *
 * - agent_end が cause: "error" で終わったとき、接続チェック URL にアクセスできるか確認する。
 * - オフラインなら一定間隔で疎通チェックを続け、オンラインに復帰したら
 *   「中断した作業を再開せよ」というメッセージを inject して自動で再試行する。
 * - 回線は生きていてサーバーが一時的なエラー（429・5xx・overloaded など）を返し続けたときは、
 *   SDK の再試行（約35秒）を使い切った後も、間隔を空けて（既定 30・60・120・120 秒）再試行する。
 *   待っている間にユーザーが入力したら、その再試行はやめる。
 * - /reconnect コマンドで手動でも同じ処理を起こせる（status で状態確認のみ）。
 *
 * 設定（config の "plugins": { "settings": { "auto-reconnect": { ... } } }）:
 * - checkUrl:         疎通チェック先 URL（応答が1つでも返ればオンラインとみなす）。既定は使用中モデルの
 *                     エンドポイント（ローカルの LLM サーバーが落ちた場合も、その復帰を待てる）
 * - checkIntervalMs:  復旧待ちのポーリング間隔（既定 5000）
 * - checkTimeoutMs:   1回の疎通チェックのタイムアウト（既定 3000）
 * - maxWaitMs:        復旧を待つ最大時間。超えたらあきらめて通知する（既定 600000 = 10分）
 * - retryMessage:     復旧後に inject するメッセージ
 * - quiet:            true にすると通知を出さない（ステータス表示と inject のみ）
 * - serverRetries:    サーバーの一時的なエラーで再試行する回数（既定 4。0 で再試行しない）
 * - serverRetryMs:    その最初の待ち時間。毎回倍にし、4倍で頭打ち（既定 30000）
 */
import { t } from "@sasacode/host";
import type { Plugin } from "@sasacode/plugin-api";

interface AutoReconnectSettings {
  checkUrl?: string;
  checkIntervalMs?: number;
  checkTimeoutMs?: number;
  maxWaitMs?: number;
  retryMessage?: string;
  quiet?: boolean;
  serverRetries?: number;
  serverRetryMs?: number;
}

/** The server answered but could not serve now: worth asking again later. 4xx other than these is the request's fault. */
export const TRANSIENT = /^(?:408|409|425|429|5\d\d)\b|overloaded|temporarily unavailable|rate.?limit/i;

const OFFICIAL_HOSTS: Record<string, string> = {
  anthropic: "https://api.anthropic.com",
  "openai-chat": "https://api.openai.com",
  "openai-responses": "https://api.openai.com",
  "openai-codex": "https://chatgpt.com",
};

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

const plugin: Plugin = (api) => {
  const s = (api.settings ?? {}) as AutoReconnectSettings;
  // The endpoint that failed is the one worth waiting for (a local LLM server, a VPN host), and
  // asking it leaks nothing to a third party. Without a baseUrl, the provider's official API.
  const target = (): string => {
    const m = api.agent.model();
    return s.checkUrl ?? m.baseUrl ?? OFFICIAL_HOSTS[m.api] ?? "https://api.openai.com";
  };
  const checkIntervalMs = s.checkIntervalMs ?? 5_000;
  const checkTimeoutMs = s.checkTimeoutMs ?? 3_000;
  const maxWaitMs = s.maxWaitMs ?? 10 * 60_000;
  const retryMessage =
    s.retryMessage ??
    "ネットワークが復旧しました。直前のエラー（接続失敗）で中断した作業を、続きから再開してください。";
  const quiet = s.quiet ?? false;
  const serverRetries = s.serverRetries ?? 4;
  const serverRetryMs = s.serverRetryMs ?? 30_000;

  let waiting = false;
  let stopped = false;
  let attempts = 0;
  let cancelWait: (() => void) | undefined;

  /** 1つでも HTTP 応答が返ればオンライン（4xx/5xx でも回線は生きている）。 */
  async function isOnline(): Promise<boolean> {
    try {
      await fetch(target(), {
        method: "GET",
        signal: AbortSignal.timeout(checkTimeoutMs),
        cache: "no-store",
        redirect: "manual",
      });
      return true;
    } catch {
      return false;
    }
  }

  async function waitForOnline(): Promise<boolean> {
    const deadline = Date.now() + maxWaitMs;
    while (!stopped && Date.now() < deadline) {
      await sleep(checkIntervalMs);
      if (await isOnline()) return true;
    }
    return false;
  }

  /** オフラインなら復旧を待ち、復帰したら inject して再試行させる。 */
  async function monitorAndRetry(trigger: "error" | "manual"): Promise<void> {
    if (waiting || stopped) return;
    if (await isOnline()) {
      if (trigger === "manual") {
        api.ui.notify(t("オンラインです。接続に問題ありません。"), "info");
        api.session.inject(
          "接続に問題ありません。直前で中断した作業があれば、続きから再開してください。",
          "now",
        );
      }
      return;
    }
    waiting = true;
    api.ui.setStatus("auto-reconnect", t("オフライン — ネットワーク復旧待ち"));
    if (!quiet) {
      api.ui.notify(t("ネットワークに接続できません（{target}）。復旧を待って自動再試行します。", { target: target() }), "warning");
    }
    const ok = await waitForOnline();
    waiting = false;
    api.ui.setStatus("auto-reconnect", undefined);
    if (stopped) return;
    if (ok) {
      if (!quiet) api.ui.notify(t("ネットワークが復旧しました。自動再試行します。"), "info");
      api.session.inject(retryMessage, "now");
    } else {
      api.ui.notify(
        t("ネットワークが復旧しませんでした（{s}秒待機）。/reconnect で手動再試行できます。", { s: Math.round(maxWaitMs / 1000) }),
        "warning",
      );
    }
  }

  /** Resolves true after `ms`, or false when cancelled (the user typed, the session ended). */
  const wait = (ms: number) =>
    new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => resolve(true), ms);
      cancelWait = () => {
        clearTimeout(timer);
        resolve(false);
      };
    }).finally(() => (cancelWait = undefined));

  /** サーバーが一時的に応答できないとき、間隔を空けて同じ作業を再試行する。 */
  async function retryServer(error: string): Promise<void> {
    if (!(await isOnline())) return monitorAndRetry("error");
    if (attempts >= serverRetries) {
      attempts = 0;
      if (serverRetries) api.ui.notify(t("サーバーのエラーが続いたため、再試行をやめました。"), "warning");
      return;
    }
    const ms = Math.min(serverRetryMs * 2 ** attempts, serverRetryMs * 4);
    attempts++;
    const s = Math.round(ms / 1000);
    waiting = true;
    api.ui.setStatus("auto-reconnect", t("サーバーの一時的なエラー — {s}秒後に再試行（{n}/{max}）", { s, n: attempts, max: serverRetries }));
    if (!quiet) api.ui.notify(t("サーバーが一時的に応答できません（{error}）。{s}秒待って再試行します。", { error: error.slice(0, 160), s }), "warning");
    const go = await wait(ms);
    waiting = false;
    api.ui.setStatus("auto-reconnect", undefined);
    if (go && !stopped) api.session.inject("サーバーの一時的なエラーで中断しました。中断した作業を、続きから再開してください。", "now");
  }

  api.on("session_start", () => {
    stopped = false;
  });

  // 待っている間にユーザーが入力したら、そちらを優先して再試行はやめる。
  api.on("user_prompt", () => {
    if (!cancelWait) return;
    cancelWait();
    attempts = 0;
  });

  api.on("session_end", () => {
    stopped = true;
    cancelWait?.();
    api.ui.setStatus("auto-reconnect", undefined);
  });

  // モデル呼び出しなどでランがエラー終了し、かつ回線が落ちていれば復旧待ちに入る。
  api.on("agent_end", ({ cause, error }) => {
    if (cause === "done") attempts = 0;
    if (cause !== "error" || waiting || stopped) return;
    // 待機中はブロックしない（バックグラウンドで監視）
    void (error && TRANSIENT.test(error) ? retryServer(error) : monitorAndRetry("error"));
  });

  api.registerCommand({
    name: "reconnect",
    description: t("ネットワーク復旧を待って接続を再試行する。引数なしで再試行、status で状態確認のみ。"),
    argumentHint: "[status]",
    complete: (prefix) =>
      ["status"]
        .filter((a) => a.startsWith(prefix))
        .map((a) => ({ value: a, label: a === "status" ? t("状態確認のみ") : a })),
    async run({ args }) {
      const arg = (args ?? "").trim();
      if (arg === "status") {
        const online = await isOnline();
        api.ui.notify(
          online
            ? t("オンラインです（{target} に到達可能）。", { target: target() })
            : t("オフラインです（{target} に接続できません）。", { target: target() }),
          online ? "info" : "warning",
        );
        return;
      }
      if (arg !== "") {
        api.ui.notify(t('不明な引数 "{arg}" です。/reconnect [status] を使ってください。', { arg }), "warning");
        return;
      }
      void monitorAndRetry("manual");
    },
  });
};

export default plugin;
