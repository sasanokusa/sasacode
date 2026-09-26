/**
 * auto-reconnect — ネットワーク障害で接続が切れたとき、復旧を検知して自動で接続を再試行する。
 *
 * - agent_end が cause: "error" で終わったとき、接続チェック URL にアクセスできるか確認する。
 * - オフラインなら一定間隔で疎通チェックを続け、オンラインに復帰したら
 *   「中断した作業を再開せよ」というメッセージを inject して自動で再試行する。
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
 */
import type { Plugin } from "@sasacode/plugin-api";

interface AutoReconnectSettings {
  checkUrl?: string;
  checkIntervalMs?: number;
  checkTimeoutMs?: number;
  maxWaitMs?: number;
  retryMessage?: string;
  quiet?: boolean;
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

const plugin: Plugin = (api) => {
  const s = (api.settings ?? {}) as AutoReconnectSettings;
  // The endpoint that failed is the one worth waiting for (a local LLM server, a VPN host), and
  // asking it leaks nothing to a third party.
  const target = (): string => s.checkUrl ?? api.agent.model().baseUrl ?? "https://www.google.com/generate_204";
  const checkIntervalMs = s.checkIntervalMs ?? 5_000;
  const checkTimeoutMs = s.checkTimeoutMs ?? 3_000;
  const maxWaitMs = s.maxWaitMs ?? 10 * 60_000;
  const retryMessage =
    s.retryMessage ??
    "ネットワークが復旧しました。直前のエラー（接続失敗）で中断した作業を、続きから再開してください。";
  const quiet = s.quiet ?? false;

  let waiting = false;
  let stopped = false;

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
        api.ui.notify("オンラインです。接続に問題ありません。", "info");
        api.session.inject(
          "接続に問題ありません。直前で中断した作業があれば、続きから再開してください。",
          "now",
        );
      }
      return;
    }
    waiting = true;
    api.ui.setStatus("auto-reconnect", "オフライン — ネットワーク復旧待ち");
    if (!quiet) {
      api.ui.notify(`ネットワークに接続できません（${target()}）。復旧を待って自動再試行します。`, "warning");
    }
    const ok = await waitForOnline();
    waiting = false;
    api.ui.setStatus("auto-reconnect", undefined);
    if (stopped) return;
    if (ok) {
      if (!quiet) api.ui.notify("ネットワークが復旧しました。自動再試行します。", "info");
      api.session.inject(retryMessage, "now");
    } else {
      api.ui.notify(
        `ネットワークが復旧しませんでした（${Math.round(maxWaitMs / 1000)}秒待機）。/reconnect で手動再試行できます。`,
        "warning",
      );
    }
  }

  api.on("session_start", () => {
    stopped = false;
  });

  api.on("session_end", () => {
    stopped = true;
    api.ui.setStatus("auto-reconnect", undefined);
  });

  // モデル呼び出しなどでランがエラー終了し、かつ回線が落ちていれば復旧待ちに入る。
  api.on("agent_end", ({ cause }) => {
    if (cause !== "error" || waiting || stopped) return;
    void monitorAndRetry("error"); // 待機中はブロックしない（バックグラウンドで監視）
  });

  api.registerCommand({
    name: "reconnect",
    description: "ネットワーク復旧を待って接続を再試行する。引数なしで再試行、status で状態確認のみ。",
    argumentHint: "[status]",
    complete: (prefix) =>
      ["status"]
        .filter((a) => a.startsWith(prefix))
        .map((a) => ({ value: a, label: a === "status" ? "状態確認のみ" : a })),
    async run({ args }) {
      const arg = (args ?? "").trim();
      if (arg === "status") {
        const online = await isOnline();
        api.ui.notify(
          online
            ? `オンラインです（${target()} に到達可能）。`
            : `オフラインです（${target()} に接続できません）。`,
          online ? "info" : "warning",
        );
        return;
      }
      if (arg !== "") {
        api.ui.notify(`不明な引数 "${arg}" です。/reconnect [status] を使ってください。`, "warning");
        return;
      }
      void monitorAndRetry("manual");
    },
  });
};

export default plugin;
