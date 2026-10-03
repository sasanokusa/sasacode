import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join } from "node:path";
import { pathToFileURL } from "node:url";
import plugin, { clipboardImage, findRefs, sniff, words } from "../src/image-attach.ts";
import { loadPlugin } from "../../../plugins/test-support.ts";

const root = mkdtempSync(join(tmpdir(), "image-attach-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64");
const B64 = PNG.toString("base64");

function fixture() {
  const cwd = mkdtempSync(join(root, "p-"));
  writeFileSync(join(cwd, "shot.png"), PNG);
  mkdirSync(join(cwd, "my dir"));
  writeFileSync(join(cwd, "my dir", "a b.png"), PNG);
  writeFileSync(join(cwd, "fake.png"), "not an image");
  return cwd;
}
async function prompt(h: Awaited<ReturnType<typeof loadPlugin>>, input: string) {
  let content: any[] = [{ type: "text", text: input }];
  await h.agent.hooks.run("user_prompt", { content }, (r) => { if (r.content) content = r.content; });
  return content;
}
const kinds = (c: any[]) => c.map((b) => b.type);

test("findRefs: @path is explicit, a bare path needs to be absolute; text after the extension is left alone", () => {
  const p = (s: string) => findRefs(s).map((r) => [r.path, r.explicit]);
  expect(p("見て @shot.pngを確認")).toEqual([["shot.png", true]]);
  expect(p("look at /tmp/a\\ b.png now")).toEqual([["/tmp/a b.png", false]]);
  expect(p("'/tmp/a b.PNG' and \"~/x.jpg\"")).toEqual([["/tmp/a b.PNG", false], ["~/x.jpg", false]]);
  expect(p("@\"my dir/a b.png\"")).toEqual([["my dir/a b.png", true]]);
  expect(p("file:///tmp/x.webp")).toEqual([["file:///tmp/x.webp", false]]);
  // Prose that merely mentions a name, an e-mail, a non-image or a URL is not an attachment.
  expect(p("shot.png is bad; a@b.png; /tmp/x.txt; https://x.com/a.png; app.png2")).toEqual([]);
});

test("sniff and words", () => {
  expect(sniff(PNG)).toBe("image/png");
  expect(sniff(Buffer.from("GIF89a"))).toBe("image/gif");
  expect(sniff(Buffer.from("RIFF\0\0\0\0WEBPVP8 "))).toBe("image/webp");
  expect(sniff(Buffer.from([0xff, 0xd8, 0xff, 0xe0]))).toBe("image/jpeg");
  expect(sniff(Buffer.from("hello"))).toBeUndefined();
  expect(words(`a "b c" d\\ e 'f g'`)).toEqual(["a", "b c", "d e", "f g"]);
});

test("@path and a dropped absolute path become images after the text", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  const abs = join(cwd, "my dir", "a b.png");
  // What a drop pastes: `\ ` for a space, or on Windows the path in quotes.
  const dropped = process.platform === "win32" ? `"${abs}"` : abs.replace(/ /g, "\\ ");
  const c = await prompt(h, `これ @shot.pngと ${dropped} を比べて`);
  expect(kinds(c)).toEqual(["text", "image", "image"]);
  expect(c[0].text).toBe(`これ [Image 1: ${join(cwd, "shot.png")}]と [Image 2: ${abs}] を比べて`);
  expect(c[1]).toEqual({ type: "image", mediaType: "image/png", data: B64 });
});

test("~ and file:// are resolved; a bare relative name is left as text", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  expect(kinds(await prompt(h, pathToFileURL(join(cwd, "shot.png")).href))).toEqual(["text", "image"]);
  expect(kinds(await prompt(h, "shot.png はどう？"))).toEqual(["text"]);
  const home = join(homedir(), `.image-attach-test-${process.pid}.png`);
  writeFileSync(home, PNG);
  try { expect(kinds(await prompt(h, `~/${basename(home)}`))).toEqual(["text", "image"]); } finally { rmSync(home); }
});

test("a failed @ is reported and stays text; a dropped non-file is silent; fake images are refused", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  expect(kinds(await prompt(h, "@missing.png @fake.png"))).toEqual(["text"]);
  expect(h.notices.length).toBe(2);
  const before = h.notices.length;
  expect(kinds(await prompt(h, `${cwd}/missing.png`))).toEqual(["text"]);
  expect(h.notices.length).toBe(before);
});

test("an image over 5 MB is refused with a hint", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  writeFileSync(join(cwd, "big.png"), Buffer.concat([PNG, Buffer.alloc(5_000_001)]));
  expect(kinds(await prompt(h, "@big.png"))).toEqual(["text"]);
  expect(h.notices.join()).toContain("sips");
});

test("images already in the prompt are kept, and no refs means the hook does nothing", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  const existing = { type: "image", mediaType: "image/png", data: "x" };
  let content: any[] = [existing, { type: "text", text: "plain" }];
  let changed = false;
  await h.agent.hooks.run("user_prompt", { content }, (r) => { if (r.content) changed = true; });
  expect(changed).toBe(false);
  content = [existing, { type: "text", text: "@shot.png" }];
  await h.agent.hooks.run("user_prompt", { content }, (r) => { if (r.content) content = r.content; });
  expect(kinds(content)).toEqual(["image", "text", "image"]);
});

test("the limit and dropPaths=false", async () => {
  const cwd = fixture();
  const h = await loadPlugin("image-attach", plugin, cwd, { maxImages: 1 });
  expect(kinds(await prompt(h, "@shot.png @shot.png"))).toEqual(["text", "image"]);
  const off = await loadPlugin("image-attach", plugin, cwd, { dropPaths: false });
  expect(kinds(await prompt(off, `${cwd}/shot.png`))).toEqual(["text"]);
  expect(kinds(await prompt(off, "@shot.png"))).toEqual(["text", "image"]);
});

test("/image queues an image for the next message only; clear drops it", async () => {
  const cwd = fixture(); const h = await loadPlugin("image-attach", plugin, cwd);
  const cmd = h.host.commands.find((c) => c.name === "image")!;
  await cmd.run({ args: `shot.png "my dir/a b.png"` });
  expect(h.host.status.get("image-attach:image-attach")).toBe("📎 2");
  const first = await prompt(h, "what?");
  expect(kinds(first)).toEqual(["text", "text", "text", "image", "image"]);
  expect(h.host.status.size).toBe(0);
  expect(kinds(await prompt(h, "again"))).toEqual(["text"]);
  await cmd.run({ args: "shot.png" }); await cmd.run({ args: "clear" });
  expect(kinds(await prompt(h, "x"))).toEqual(["text"]);
  await cmd.run({ args: "fake.png" });
  expect(h.notices.at(-1)).toContain("not a PNG");
});

test("clipboard: Linux reads stdout; macOS goes through a file the script writes; empty is a clear error", async () => {
  const seen: string[] = [];
  expect((await clipboardImage("linux", async (cmd) => { seen.push(cmd); if (cmd === "wl-paste") throw new Error("nope"); return PNG; })).data).toBe(B64);
  expect(seen).toEqual(["wl-paste", "xclip"]);
  await expect(clipboardImage("linux", async () => Buffer.alloc(0))).rejects.toThrow("no image");
  const mac = await clipboardImage("darwin", async (_cmd, args) => {
    writeFileSync(/POSIX file "([^"]+)"/.exec(args.join("\n"))![1]!, PNG); return Buffer.alloc(0);
  });
  expect(mac.mediaType).toBe("image/png");
  await expect(clipboardImage("darwin", async () => { throw new Error("Command failed: osascript"); })).rejects.toThrow("no image");
  const win = await clipboardImage("win32", async (cmd, args) => {
    expect(cmd).toBe("powershell.exe");
    writeFileSync(/Save\('([^']+)'/.exec(args.join(" "))![1]!.replace(/''/g, "'"), PNG); return Buffer.alloc(0);
  });
  expect(win.mediaType).toBe("image/png");
  await expect(clipboardImage("win32", async () => { throw new Error("Command failed: powershell.exe"); })).rejects.toThrow("no image");
  await expect(clipboardImage("freebsd")).rejects.toThrow("not supported");
});
