import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { t } from "@sasacode/host";
import { text, type Plugin, type UserContent } from "@sasacode/plugin-api";

type Image = Extract<UserContent, { type: "image" }>;
/** The providers' limit for one image (Anthropic's is 5 MB); a larger one would fail every request after it. */
const MAX_BYTES = 5_000_000;
const EXT = "png|jpe?g|gif|webp";
// `@path`, a quoted path, or a bare word; a bare word may carry `\ `-escaped spaces (what terminals paste on a drop).
const TOKEN = /(@)?(?:"([^"\n]+)"|'([^'\n]+)'|((?:\\.|[^\s"'\\])+))/g;

export interface Ref { start: number; end: number; path: string; explicit: boolean }

/**
 * Image paths in a prompt. `@path` (any path) is explicit; without the `@` only an absolute, `~/`
 * or `file://` path counts, which is what dropping a file into the terminal pastes.
 */
export function findRefs(s: string): Ref[] {
  const refs: Ref[] = [];
  for (const m of s.matchAll(TOKEN)) {
    const explicit = !!m[1];
    let raw: string | undefined;
    let end = m.index + m[0].length;
    const quoted = m[2] ?? m[3];
    if (quoted !== undefined) {
      if (new RegExp(`\\.(${EXT})$`, "i").test(quoted)) raw = quoted;
    } else {
      // Japanese text often has no space after a path: stop at the extension.
      const e = new RegExp(`^(.*?\\.(?:${EXT}))(?![A-Za-z0-9_])`, "i").exec(m[4]!);
      if (e) {
        raw = e[1]!.replace(/\\(.)/g, "$1");
        end = m.index + (explicit ? 1 : 0) + e[1]!.length;
      }
    }
    if (raw === undefined) continue;
    if (!explicit && !/^(\/|~\/|file:\/\/)/.test(raw)) continue;
    refs.push({ start: m.index, end, path: raw, explicit });
  }
  return refs;
}

export function resolvePath(p: string, cwd: string): string {
  if (p.startsWith("file://")) p = decodeURIComponent(new URL(p).pathname);
  if (p === "~" || p.startsWith("~/")) p = join(homedir(), p.slice(1));
  return resolve(cwd, p);
}

/** The type from the bytes, not the name: a wrong media type would fail every later request. */
export function sniff(b: Uint8Array): string | undefined {
  const at = (i: number, s: string) => [...s].every((c, k) => b[i + k] === c.charCodeAt(0));
  if (b[0] === 0x89 && at(1, "PNG")) return "image/png";
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return "image/jpeg";
  if (at(0, "GIF8")) return "image/gif";
  if (at(0, "RIFF") && at(8, "WEBP")) return "image/webp";
  return undefined;
}

export function toImage(buf: Buffer): Image {
  const mediaType = sniff(buf);
  if (!mediaType) throw new Error("not a PNG, JPEG, GIF or WebP image");
  if (buf.length > MAX_BYTES)
    throw new Error(`${buf.length} bytes, over the ${MAX_BYTES} an image may have (make a smaller copy, e.g. sips -Z 1600)`);
  return { type: "image", mediaType, data: buf.toString("base64") };
}

export async function loadImage(path: string): Promise<Image> {
  const st = await stat(path);
  if (!st.isFile()) throw new Error("not a file");
  if (st.size > MAX_BYTES) throw new Error(`${st.size} bytes, over the ${MAX_BYTES} an image may have (make a smaller copy, e.g. sips -Z 1600)`);
  return toImage(await readFile(path));
}

export type Run = (cmd: string, args: string[]) => Promise<Buffer>;
const run: Run = (cmd, args) =>
  new Promise((ok, fail) => execFile(cmd, args, { encoding: "buffer", maxBuffer: 32 * 1024 * 1024, timeout: 10_000 }, (e, out) => (e ? fail(e) : ok(out))));

/** The image on the system clipboard: macOS via osascript, Linux via wl-paste or xclip. */
export async function clipboardImage(platform = process.platform, exec: Run = run): Promise<Image> {
  if (platform === "darwin") {
    const dir = await mkdtemp(join(tmpdir(), "sasacode-clip-"));
    const file = join(dir, "clip.png");
    const path = file.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    const script = [
      "try", "set d to the clipboard as «class PNGf»", "on error", 'error "no image"', "end try",
      `set fh to open for access POSIX file "${path}" with write permission`,
      "set eof of fh to 0", "write d to fh", "close access fh",
    ];
    try {
      await exec("osascript", script.flatMap((l) => ["-e", l]));
      return await loadImage(file);
    } catch (e) {
      if (e instanceof Error && !/no image|Command failed/.test(e.message)) throw e;
      throw new Error("the clipboard has no image");
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }
  if (platform === "linux") {
    for (const [cmd, args] of [["wl-paste", ["--type", "image/png"]], ["xclip", ["-selection", "clipboard", "-t", "image/png", "-o"]]] as const) {
      const out = await exec(cmd, [...args]).catch(() => undefined);
      if (out?.length) return toImage(out);
    }
    throw new Error("the clipboard has no image (wl-paste or xclip is needed)");
  }
  throw new Error(`the clipboard is not supported on ${platform}`);
}

/** Words of a command line, honoring quotes and `\ `. */
export function words(s: string): string[] {
  return [...s.matchAll(/"([^"]*)"|'([^']*)'|((?:\\.|\S)+)/g)].map((m) => m[1] ?? m[2] ?? m[3]!.replace(/\\(.)/g, "$1"));
}

const plugin: Plugin = (api) => {
  const settings = api.defineSettings<{ dropPaths: boolean; maxImages: number }>({
    schema: {
      type: "object",
      properties: { dropPaths: { type: "boolean" }, maxImages: { type: "integer", minimum: 1, maximum: 20 } },
      additionalProperties: false,
    },
    defaults: { dropPaths: true, maxImages: 8 },
  });
  let pending: { name: string; image: Image }[] = [];
  const status = () => api.ui.setStatus("image-attach", pending.length ? `📎 ${pending.length}` : undefined);

  api.on("user_prompt", async ({ content }) => {
    const attached: { name: string; image: Image }[] = [];
    const out: UserContent[] = [];
    const fail = (name: string, e: unknown) => api.ui.notify(t("画像を付けられません: {name}: {error}", { name, error: (e as Error).message }), "warning");
    for (const block of content) {
      if (block.type !== "text") { out.push(block); continue; }
      let result = "";
      let pos = 0;
      for (const r of findRefs(block.text)) {
        if (!r.explicit && !settings.dropPaths) continue;
        if (attached.length + pending.length >= settings.maxImages) break;
        const path = resolvePath(r.path, api.cwd);
        try {
          attached.push({ name: path, image: await loadImage(path) });
        } catch (e) {
          // A dropped path that is not a file is just text; a typed `@` or an oversize file is worth saying.
          if (r.explicit || (e as Error).message.includes("bytes")) fail(r.path, e);
          continue;
        }
        result += block.text.slice(pos, r.start) + `[Image ${attached.length}: ${path}]`;
        pos = r.end;
      }
      out.push(text(result + block.text.slice(pos)));
    }
    for (const p of pending.splice(0)) {
      attached.push(p);
      out.push(text(`[Image ${attached.length}: ${p.name}]`));
    }
    status();
    if (!attached.length) return;
    return { content: [...out, ...attached.map((a) => a.image)] };
  });

  api.registerCommand({
    name: "image",
    description: t("画像を次のメッセージに添付（引数なし: クリップボード、clear: 取り消し）"),
    argumentHint: "[path… | clear]",
    async run({ args }) {
      const list = words(args);
      if (list[0] === "clear") {
        pending = [];
        status();
        api.ui.notify(t("添付を取り消しました"));
        return;
      }
      for (const w of list.length ? list : [undefined]) {
        if (pending.length >= settings.maxImages) { api.ui.notify(t("画像は{n}枚までです", { n: settings.maxImages }), "warning"); break; }
        const name = w === undefined ? "clipboard" : resolvePath(w, api.cwd);
        try {
          pending.push({ name, image: w === undefined ? await clipboardImage() : await loadImage(name) });
        } catch (e) {
          api.ui.notify(t("画像を付けられません: {name}: {error}", { name, error: (e as Error).message }), "error");
        }
      }
      status();
      if (pending.length) api.ui.notify(t("画像 {n} 枚を次のメッセージに添付します", { n: pending.length }));
    },
  });

  api.on("session_end", () => { pending = []; status(); });
};
export default plugin;
