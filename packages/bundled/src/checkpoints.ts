/**
 * checkpoints — write / edit が変えたファイルの直前の内容をセッションに退避し、/undo で戻す。
 *
 *   - 変更の直前に内容を控え、呼び出しが本当に成功したときだけチェックポイントとして保存する
 *   - /undo で直近の1件（1回の呼び出しが変えたファイル全部）を戻す。繰り返すごとに1件ずつ遡る
 *   - 退避はセッションログに残るので、/resume のあとも戻せる
 *
 * 設定（config の "plugins": { "settings": { "checkpoints": { ... } } }）:
 * - maxFileKB: 退避するファイルの上限（既定 1024）。大きいもの・テキストでないものは退避せず、
 *   /undo のときに「戻せない」と伝える。bash の変更は対象外（何を書き換えたか分からない）。
 */
import { existsSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Plugin } from "@sasacode/plugin-api";

interface Change {
  path: string;
  /** 退避した内容（戻せるときだけ）。 */
  before?: string;
  /** ファイルは元からなかった: 戻すときは削除する。 */
  created?: boolean;
  /** 退避していない（大きい・テキストでない）: 戻せないと伝えるだけ。 */
  skipped?: boolean;
}

/** 1回のツール呼び出しの退避。呼び出し単位でまとめて戻す。 */
interface Checkpoint {
  id: string;
  tool: string;
  at: number;
  changes: Change[];
}

/** 追記だけの記録: 退避そのものと、それを戻したという印。 */
type Entry = Checkpoint | { undone: string };

const KIND = "checkpoint";
const DEFAULT_MAX_KB = 1024;

const plugin: Plugin = (api) => {
  const maxKB = (api.settings.maxFileKB as number | undefined) ?? DEFAULT_MAX_KB;
  /** 結果が分かるまで、呼び出しごとの退避。 */
  const pending = new Map<string, Change[]>();

  /** 戻していないチェックポイントだけ、古い順に。 */
  const stack = (): Checkpoint[] => {
    const all = (api.session.entries(KIND) as Entry[]).filter((e) => !!e && typeof e === "object");
    const gone = new Set(all.filter((e): e is { undone: string } => "undone" in e).map((e) => e.undone));
    return all.filter((e): e is Checkpoint => "changes" in e && !gone.has(e.id));
  };

  const snapshot = (path: string): Change => {
    try {
      if (!existsSync(path)) return { path, created: true };
      if (statSync(path).size > maxKB * 1024) return { path, skipped: true };
      const before = readFileSync(path, "utf8");
      return before.includes("\0") ? { path, skipped: true } : { path, before }; // not text: restoring it as text would corrupt it
    } catch {
      return { path, skipped: true };
    }
  };

  api.on("tool_call", ({ call, tool, args }) => {
    if (tool.kind !== "edit") return; // bash など、変えたものが分からないものもここで除かれる
    const paths = tool.paths?.(args, api.cwd) ?? [];
    if (!paths.length) return;
    pending.set(call.id, paths.map((p) => snapshot(resolve(api.cwd, p))));
  });

  api.on("tool_result", ({ call, result, ran }) => {
    const changes = pending.get(call.id);
    pending.delete(call.id);
    if (!changes || !ran) return;
    // 実際に変わったものだけ残す: 失敗した呼び出しや無変更の書き込みが /undo の邪魔をしないように。
    const keep = changes.filter((ch) => {
      if (ch.skipped) return !result.isError;
      if (ch.created) return existsSync(ch.path);
      try {
        return readFileSync(ch.path, "utf8") !== ch.before;
      } catch {
        return true; // なくなったり読めなくなったりした: 戻せるに越したことはない
      }
    });
    if (keep.length) api.session.append(KIND, { id: call.id, tool: call.name, at: Date.now(), changes: keep } satisfies Checkpoint);
  });

  /** 1件の変更を巻き戻す。ファイルごとに1行の報告。 */
  const restore = (c: Checkpoint): string[] =>
    c.changes.map((ch) => {
      try {
        if (ch.skipped) return `${ch.path}: 内容を退避していないため戻せません`;
        if (ch.created) {
          if (!existsSync(ch.path)) return `${ch.path}: すでにありません`;
          rmSync(ch.path);
          return `${ch.path}: 削除しました（変更前はなかったため）`;
        }
        writeFileSync(ch.path, ch.before ?? "");
        return `${ch.path}: 戻しました`;
      } catch (e) {
        return `${ch.path}: 戻せませんでした（${e instanceof Error ? e.message : e}）`;
      }
    });

  api.registerCommand({
    name: "undo",
    description: "write / edit によるファイル変更を1件戻す",
    argumentHint: "[list | <path>]",
    complete: (prefix: string) => ["list"].filter((s) => s.startsWith(prefix ?? "")).map((value) => ({ value, label: value })),
    run({ args }: { args?: string }) {
      const a = (args ?? "").trim();
      const left = stack();
      if (a === "list") {
        if (!left.length) return api.ui.notify("[undo] 戻せる変更はありません");
        const lines = left.map((c, i) => `${left.length - i}. ${new Date(c.at).toLocaleTimeString()} ${c.tool} — ${c.changes.map((ch) => ch.path).join(", ")}`);
        return api.ui.notify(`[undo] 戻せる変更（新しい順）:\n${lines.reverse().join("\n")}`);
      }
      const pick = a ? left.filter((c) => c.changes.some((ch) => ch.path === resolve(api.cwd, a) || ch.path.endsWith(a))).at(-1) : left.at(-1);
      if (!pick) return api.ui.notify(a ? `[undo] ${a} の変更が見つかりません` : "[undo] 戻せる変更はありません", "warning");
      // 戻した先からまた戻せる（＝変更の前後が入れ替わる）と迷うので、1件は消費したことにする。
      api.session.append(KIND, { undone: pick.id } satisfies Entry);
      api.ui.notify(`[undo] 戻しました:\n${restore(pick).join("\n")}`);
    },
  });
};

export default plugin;
