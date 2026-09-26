/**
 * goal — セッション単位の「ゴール（最終目的）」を人間とエージェントの両方から設定する
 *
 *   - 人間は /goal コマンド、エージェントは set_goal ツールで設定する
 *   - 状態はセッションエントリにのみ保存する（ファイルには何も書かない）
 *   - ゴールは1セッションに1件だけ。置き換えた古いゴールは残さない
 */
import { errorResult, text, type Plugin } from "@sasacode/plugin-api";

type Status = "active" | "paused" | "done";
type Source = "human" | "agent";

interface GoalState {
  text: string;
  status: Status;
  source: Source;
  updatedAt: string;
}

const STATUS_LABEL: Record<Status, string> = { active: "作業中", paused: "一時停止", done: "達成" };
const SOURCE_LABEL: Record<Source, string> = { human: "/goal コマンド", agent: "set_goal ツール" };
const ENTRY_KIND = "goal-snapshot";
const FOOTER_CHARS = 50;

const USAGE = [
  "使い方:",
  "  /goal                   現在のゴールを表示",
  "  /goal <テキスト>        ゴールを設定",
  "  /goal clear             ゴールを削除",
  "  /goal done              達成とした",
  "  /goal pause             一時停止",
  "  /goal resume            作業中に戻す",
  "  /goal help              この表示",
  "",
  "エージェントは set_goal ツールで同じことができます。",
].join("\n");

/** モデルに渡す依頼文: ゴール設定の申し出は正規表現では見ない。モデルに判断させる。 */
const CONTRACT = [
  "[goal] このセッションのゴール（最終目的）について:",
  "- エンドユーザーが「これをgoalに設定して」「目的を固定して」「最終目的はこれ」のように",
  "  ゴール・目標の設定や変更を頼んだときは、set_goal ツールを呼んで設定する。",
  "  ゴールの中身は会話から要約して1〜2文で書く。作業手順の列ではなく、到達すべき最終状態を書く。",
  "- ゴールに到達したら set_goal(status: \"done\")、一時的に置くなら set_goal(status: \"paused\")。",
  "- ゴールを確認したいときは goal_status ツールを使う。",
].join("\n");

const plugin: Plugin = (api) => {
  let goal: GoalState | null = null;

  // ------------------------------------------------------------ 永続化

  // null records a cleared goal, so /resume does not bring it back.
  const persist = (): void => api.session.append(ENTRY_KIND, goal);

  api.on("session_start", () => {
    const entries = api.session.entries(ENTRY_KIND);
    const last = entries[entries.length - 1];
    goal = last && typeof last === "object" && typeof (last as GoalState).text === "string" ? (last as GoalState) : null;
    paint();
  });

  // ------------------------------------------------------------ 状態変更の唯一の入口

  /** text があれば内容も変える。status だけなら状態のみ動かす。両方無いは false。 */
  function change(next: { text?: string; status?: Status }, source: Source): boolean {
    const body = next.text?.trim() ?? "";
    const status = next.status ?? "active";

    if (body) {
      goal = { text: body, status, source, updatedAt: new Date().toISOString() };
    } else if (goal) {
      goal = { ...goal, status, updatedAt: new Date().toISOString() };
    } else {
      return false;
    }
    persist();
    paint();
    return true;
  }

  function clear(): void {
    goal = null;
    persist();
    paint();
  }

  function render(): string {
    if (!goal) return "ゴールは未設定です。人間は /goal、エージェントは set_goal ツールで設定できます。";
    return `🎯 ゴール: ${goal.text}\n  状態: ${STATUS_LABEL[goal.status]}　設定元: ${SOURCE_LABEL[goal.source]}\n  更新: ${new Date(goal.updatedAt).toLocaleString()}`;
  }

  function paint(): void {
    const show = goal && goal.status === "active" ? goal.text : null;
    if (!show) return api.ui.setStatus("goal", undefined);
    const cut = show.length > FOOTER_CHARS ? `${show.slice(0, FOOTER_CHARS)}…` : show;
    api.ui.setStatus("goal", `🎯 ${cut}`);
  }

  // ------------------------------------------------------------ ツール

  api.registerTool({
    name: "set_goal",
    description:
      "Set, update, or change the status of this session's single goal (the final purpose of the current work). " +
      "Call it when the user asks you to set, fix, or change a goal, purpose, or final purpose " +
      "(for example 「これをgoalに設定して」「目的を固定して」): distil the purpose out of the conversation " +
      "and pass it as one or two sentences. Write the end state to be reached, not the list of steps. " +
      "Pass status 'done' when the goal has been reached, 'paused' to park it, 'active' to resume it. " +
      "Pass only status (no text) to move the existing goal without rewriting its text.",
    parameters: {
      type: "object",
      properties: {
        text: {
          type: "string",
          description: "The goal as one or two self-contained sentences: the end state to reach, not the steps to get there.",
        },
        status: {
          type: "string",
          enum: ["active", "paused", "done"],
          description: "'active' (default) = still being worked on. 'done' = reached. 'paused' = parked.",
        },
      },
      additionalProperties: false,
    },
    kind: "other",
    concurrent: false,
    summary: (a: { text?: string; status?: Status }) =>
      `goal${a.status && a.status !== "active" ? `:${a.status}` : ""} ${(a.text ?? "").slice(0, 60) || "(status only)"}`,
    async execute(args: { text?: string; status?: Status }) {
      const body = args.text?.trim() ?? "";
      if (!body && !args.status) {
        return errorResult("set_goal: pass text to set a goal, or status to change the status of the current one.");
      }
      const status: Status = args.status ?? "active";
      if (!change({ text: body || undefined, status }, "agent")) {
        return errorResult("set_goal: no goal is set yet, so status alone cannot be applied. Pass text to set one.");
      }
      const verb = !body ? "状態を変更" : "設定";
      return { content: [text(`[goal] ${verb}しました。\n${render()}`)], details: { goal } };
    },
  });

  api.registerTool({
    name: "goal_status",
    description:
      "Read the current session goal: its text, its status, who set it, and when. Call it when you need to check " +
      "what the current purpose is, or before reporting that the work is finished.",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    kind: "read",
    async execute() {
      if (!goal) return { content: [text("ゴールは未設定です。")] };
      return { content: [text(render())], details: { goal } };
    },
  });

  // ------------------------------------------------------------ system prompt

  api.on("system_prompt", ({ prompt }: { prompt: string }) => {
    const extra = [CONTRACT];
    if (goal?.status === "active") extra.push(`現在のゴール: ${goal.text}\n作業はこのゴールに向けて進める。`);
    return { prompt: `${prompt}\n\n${extra.join("\n\n")}` };
  });

  // ------------------------------------------------------------ /goal コマンド

  const SUBS = ["clear", "done", "pause", "resume", "help"];

  api.registerCommand({
    name: "goal",
    description: "このセッションのゴール（最終目的）を設定・表示・達成・一時停止・削除する",
    argumentHint: "[<目的>|clear|done|pause|resume|help]",
    complete: (prefix: string) => {
      const p = (prefix ?? "").toLowerCase();
      return (p ? SUBS.filter((s) => s.startsWith(p)) : SUBS).map((value) => ({ value, label: value }));
    },
    async run({ args }: { args?: string }) {
      const a = (args ?? "").trim();
      const low = a.toLowerCase();

      if (!a) return api.ui.notify(render(), "info");
      if (low === "help" || low === "-h" || low === "--help") return api.ui.notify(USAGE, "info");

      if (low === "clear") {
        if (!goal) return api.ui.notify("[goal] 削除するゴールがありません（未設定です）", "warning");
        const gone = goal.text;
        clear();
        return api.ui.notify(`[goal] 削除しました: ${gone}`, "info");
      }

      if (low === "done" || low === "pause" || low === "resume") {
        const status: Status = low === "done" ? "done" : low === "pause" ? "paused" : "active";
        if (!change({ status }, "human")) return api.ui.notify("[goal] 対象のゴールがありません（未設定です）", "warning");
        return api.ui.notify(`[goal] ${STATUS_LABEL[status]}としました: ${goal?.text}`, "info");
      }

      if (!change({ text: a, status: "active" }, "human")) {
        return api.ui.notify("[goal] 設定する内容が読み取れません（空です）", "error");
      }
      api.ui.notify(`[goal] 設定しました:\n${render()}`, "info");
      api.session.inject(`[goal] ゴールを設定しました: ${goal?.text}\nこのゴールに向けて作業を進めてください。`, "now");
    },
  });

  paint();
};

export default plugin;
