import { spawn } from "node:child_process";
import { basename } from "node:path";
import { t } from "@sasacode/host";
import { childEnv, type Plugin } from "@sasacode/plugin-api";

/** How to get the window's program while it is not released with sasacode (docs/gui.md). `--locked`: the versions CI builds with. */
const INSTALL = "cargo install --locked --path crates/sasacode-gui";
/**
 * How long a window has to stay up to count as opened: time for one to turn its arguments down. A
 * guess: a program started for the first time can take longer, and is then taken for opened.
 */
const GRACE_MS = 600;

/**
 * The sasacode a window opened from this one runs: this one, when it is a program of its own. From
 * source the program is bun, and the window finds sasacode as it does by itself; so it does when
 * SASACODE_BIN says which one to use.
 */
export function ownProgram(exec = process.execPath, env: Record<string, string | undefined> = process.env): string | undefined {
  if (env.SASACODE_BIN) return undefined;
  return /^bun(\.exe)?$/.test(basename(exec)) ? undefined : exec;
}

export interface GuiTarget {
  /** The folder to work in. */
  cwd: string;
  /** The conversation to open on (`sasacode-gui --session`). */
  session?: string;
  /** The sasacode the window runs (`sasacode-gui --sasacode`). */
  program?: string;
}

/** The window's arguments (`sasacode-gui --help`). */
export function guiArgs(o: GuiTarget): string[] {
  return [...(o.program ? ["--sasacode", o.program] : []), ...(o.session ? ["--session", o.session] : []), o.cwd];
}

/**
 * Starts the window on its own: it stays when this sasacode ends. `command` is the program to start
 * (default: sasacode-gui on PATH, looked up now, so one installed a moment ago is found). Returns
 * the program once it has stayed up for a moment. One that leaves at once with an error (an older
 * sasacode-gui that does not know an option) throws: nothing the window says is seen from here.
 */
export async function openGui(o: GuiTarget & { command?: string; graceMs?: number }): Promise<string> {
  // PATH as it is now: Bun.which alone reads the one this process started with.
  const command = o.command || Bun.which("sasacode-gui", { PATH: process.env.PATH ?? "" });
  if (!command)
    throw new Error(
      t("sasacode-guiが見つかりません。sasacodeのリポジトリで次を実行すると入ります。\n  {install}\n別の場所にある場合はplugins.settings.gui.commandを指定してください。", {
        install: INSTALL,
      }),
    );
  const failed = (reason: string) => new Error(t("sasacode-guiを起動できませんでした（{command}）: {reason}", { command, reason }));
  // Detached, as a background job is: a child that is not is ended with sasacode on Windows too.
  // Nothing is read from it: a pipe nobody reads would stop it, or end it with us.
  let child: ReturnType<typeof spawn>;
  try {
    child = spawn(command, guiArgs(o), { cwd: o.cwd, detached: true, stdio: "ignore", env: childEnv() });
  } catch (e) {
    throw failed((e as Error).message);
  }
  child.unref();
  const left = await new Promise<{ error: string } | { status: number | string } | undefined>((resolve) => {
    child.once("error", (e) => resolve({ error: e.message }));
    child.once("exit", (code, signal) => resolve({ status: code ?? signal ?? "?" }));
    setTimeout(() => resolve(undefined), o.graceMs ?? GRACE_MS);
  });
  if (left && "error" in left) throw failed(left.error);
  // A launcher that starts the window and leaves with 0 has done its work.
  if (left && left.status !== 0)
    throw new Error(
      t("sasacode-guiがすぐに終了しました（{command}、終了コード{status}）。このsasacodeより古い版かもしれません。入れ直すには次を実行してください。\n  {install}", {
        command,
        status: String(left.status),
        install: INSTALL,
      }),
    );
  return command;
}

/**
 * `/gui`: the desktop window (sasacode-gui, docs/gui.md) on this folder, opened on this
 * conversation. The window runs sasacode itself for each message, so from then on the conversation
 * goes on in one of the two: neither reads what the other adds.
 *
 * settings: { command?: string }
 */
const gui: Plugin = (api) => {
  api.registerCommand({
    name: "gui",
    description: t("デスクトップGUIでこの会話を開く"),
    run: async () => {
      // A conversation nobody has said anything in has an id and no log yet: nothing to open on.
      const session = api.session.messages().length ? api.session.id : undefined;
      try {
        await openGui({ command: api.settings.command as string | undefined, cwd: api.cwd, session, program: ownProgram() });
      } catch (e) {
        api.ui.notify((e as Error).message, "error");
        return;
      }
      api.ui.notify(
        session
          ? t("GUIでこの会話を開きました。この画面とGUIは互いの発言を読み込みません。続きはどちらか一方で進めてください。")
          : t("GUIを開きました。"),
      );
    },
  });
};
export default gui;
