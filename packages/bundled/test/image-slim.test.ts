// image-slim: images shrunk with Bun.Image (every OS the tests run on), and old images left out of a request.
import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";
import type { Message } from "@sasacode/ai";
import plugin, { omit, opaque, probe, shrink } from "../src/image-slim.ts";
import { loadPlugin } from "../../../plugins/test-support.ts";

const root = mkdtempSync(join(tmpdir(), "image-slim-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

/** A PNG of w×h noise (so it compresses badly, like a screenshot's detail); RGBA when `alpha`, every pixel opaque with "opaque". */
function png(w: number, h: number, alpha: boolean | "opaque" = false) {
  const ch = alpha ? 4 : 3;
  const raw = Buffer.alloc((w * ch + 1) * h);
  let x = 1;
  for (let i = 0; i < raw.length; i++) {
    const k = (i % (w * ch + 1)) - 1;
    raw[i] = k < 0 ? 0 : alpha === "opaque" && k % 4 === 3 ? 255 : (x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 24;
  }
  const chunk = (type: string, data: Buffer) => {
    const t = Buffer.concat([Buffer.from(type, "latin1"), data]);
    const len = Buffer.alloc(4), crc = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    crc.writeUInt32BE(crc32(t));
    return Buffer.concat([len, t, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = alpha ? 6 : 2;
  const file = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
  return { type: "image" as const, mediaType: "image/png", data: file.toString("base64") };
}
const S = { maxPixels: 1_048_576, jpegQuality: 90, minBytes: 100_000 };
const bytes = (img: { data: string }) => Buffer.from(img.data, "base64");

test("a large screenshot becomes a JPEG of about maxPixels, aspect ratio kept, with a note to scale positions back", async () => {
  const r = (await shrink(png(1500, 950), S))!;
  expect(r.mediaType).toBe("image/jpeg");
  const p = probe(bytes(r))!;
  expect(p.kind).toBe("jpeg");
  expect(p.w * p.h).toBeLessThanOrEqual(S.maxPixels);
  expect(p.w * p.h).toBeGreaterThan(S.maxPixels * 0.99);
  expect(Math.abs(p.w / p.h - 1500 / 950)).toBeLessThan(0.01);
  expect(r.note).toMatch(/^1500x950 PNG \d+ KB -> 1286x814 JPEG q90 \d+ KB; multiply positions in it by 1\.17 for the original$/);
});

test("a JPEG turned by its EXIF orientation is shrunk upright, its aspect ratio kept (#93)", async () => {
  const jpg = Buffer.from(await new Bun.Image(bytes(png(1600, 1200))).jpeg({ quality: 90 }).bytes());
  // APP1 Exif: a big-endian TIFF whose IFD0 has one entry, Orientation (0x0112) = 6: shown turned 90°, 1200×1600.
  const exif = Buffer.concat([Buffer.from("Exif\0\0", "latin1"), Buffer.from([0x4d, 0x4d, 0, 0x2a, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0])]);
  const app1 = Buffer.from([0xff, 0xe1, (exif.length + 2) >> 8, (exif.length + 2) & 0xff]);
  const turned = Buffer.concat([jpg.subarray(0, 2), app1, exif, jpg.subarray(2)]);
  const r = (await shrink({ type: "image", mediaType: "image/jpeg", data: turned.toString("base64") }, S))!;
  const out = await new Bun.Image(bytes(r)).metadata();
  expect([out.width, out.height]).toEqual([886, 1182]);
  expect(r.note).toStartWith("1200x1600 JPEG");
  expect(r.note).toContain("-> 886x1182 JPEG");
});

test("within maxPixels: an opaque PNG over minBytes becomes a JPEG of the same size; small, transparent and JPEG stay", async () => {
  const r = (await shrink(png(600, 400), S))!;
  expect(probe(bytes(r))).toEqual({ kind: "jpeg", w: 600, h: 400, alpha: false });
  expect(r.note).not.toContain("multiply");
  expect(await shrink(png(100, 100), S)).toBeUndefined(); // 30 KB
  expect(await shrink(png(600, 400, true), S)).toBeUndefined();
  expect(await shrink({ type: "image", mediaType: "image/jpeg", data: r.data }, S)).toBeUndefined();
  expect(await shrink({ type: "image", mediaType: "image/png", data: Buffer.from("not an image").toString("base64") }, S)).toBeUndefined();
});

test("an RGBA PNG whose every pixel is opaque (as macOS saves screenshots) becomes a JPEG; one transparent pixel keeps it a PNG", async () => {
  const shot = png(600, 400, "opaque");
  expect(probe(bytes(shot))!.alpha).toBe(true);
  expect(opaque(bytes(shot))).toBe(true);
  expect((await shrink(shot, S))!.mediaType).toBe("image/jpeg");
  // Filtered rows (Sub, Up, Average, Paeth), as encoders write them: the copy Bun.Image makes.
  const filtered = Buffer.from(await new Bun.Image(bytes(shot)).png().bytes());
  expect(opaque(filtered)).toBe(true);
  expect(opaque(bytes(png(600, 400, true)))).toBe(false);
});

test("a large transparent PNG is resized and stays a PNG", async () => {
  const r = (await shrink(png(1600, 1000, true), S))!;
  expect(r.mediaType).toBe("image/png");
  expect(probe(bytes(r))).toMatchObject({ kind: "png", alpha: true });
  expect(probe(bytes(r))!.w).toBe(1295);
});

test("omit: the oldest images go dropStep at a time, never the latest user message's; then by size", () => {
  const spots = (n: number, locked = 0, size = 1) => Array.from({ length: n }, (_, i) => ({ key: `${i}`, bytes: size, locked: i >= n - locked }));
  const s = { keepImages: 4, dropStep: 8, maxImageBytes: 100 };
  expect(omit(spots(11), s).size).toBe(0);
  expect([...omit(spots(12), s)]).toEqual(["0", "1", "2", "3", "4", "5", "6", "7"]);
  expect(omit(spots(19), s).size).toBe(8); // the prompt's start stays put between steps
  expect(omit(spots(20), s).size).toBe(16);
  expect(omit(spots(12, 6), s).size).toBe(6); // only the droppable ones
  expect(omit(spots(6, 1, 30), s).size).toBe(3); // 180 bytes: the oldest go until 90 is left
});

test("plugin: a tool's image is stored shrunk with a note; a request sends the newest and a note for the others", async () => {
  const h = await loadPlugin("image-slim", plugin, root);
  const shot = png(1500, 950);
  let result = { content: [{ type: "text" as const, text: "screenshot" }, shot] } as any;
  await h.agent.hooks.run("tool_result", { call: { type: "tool_call", id: "1", name: "read", input: {} }, result, ran: true }, (r: any) => { if (r.result) result = r.result; });
  expect(result.content.map((c: any) => c.type)).toEqual(["text", "image", "text"]);
  expect(result.content[1].mediaType).toBe("image/jpeg");
  expect(result.content[2].text).toStartWith("[image-slim: stored as a smaller copy, 1500x950 PNG");

  const small = png(10, 10);
  const messages: Message[] = [
    ...Array.from({ length: 12 }, (_, i): Message => ({ role: "tool", callId: `${i}`, toolName: "read", content: [small], isError: false, timestamp: 0 } as any)),
    { role: "user", content: [{ type: "text", text: "and this" }, shot], timestamp: 0 },
  ];
  let sent = messages;
  await h.agent.hooks.run("before_request", { messages, model: h.agent.model, sampling: {} }, (r: any) => { if (r.messages) sent = r.messages; });
  const images = sent.flatMap((m: any) => m.content.filter((c: any) => c.type === "image"));
  expect(images.length).toBe(5); // 13 images: the oldest 8 left out
  expect(sent.slice(0, 8).every((m: any) => m.content[0].text?.startsWith("[An image was here."))).toBe(true);
  expect((sent.at(-1) as any).content[1].mediaType).toBe("image/jpeg"); // the pasted one, shrunk for the request
  expect((messages.at(-1) as any).content[1]).toBe(shot); // the history is left as it was

});

test("plugin: a reminder after the user's message does not take its place: the pasted images are all sent (#94)", async () => {
  const { harnessNote } = await import("@sasacode/host");
  const h = await loadPlugin("image-slim", plugin, root);
  const small = png(10, 10);
  const messages: Message[] = [
    { role: "user", content: [{ type: "text", text: "compare these" }, ...Array.from({ length: 12 }, () => small)], timestamp: 0 },
    { role: "assistant", content: [{ type: "tool_call", id: "1", name: "read", input: {} }], timestamp: 0 } as any,
    { role: "tool", toolCallId: "1", toolName: "read", content: [{ type: "text", text: "ok" }], isError: false, timestamp: 0 },
    { role: "user", content: [{ type: "text", text: harnessNote("Your last 5 replies were tool calls with no text") }], timestamp: 0 },
  ];
  let sent = messages;
  await h.agent.hooks.run("before_request", { messages, model: h.agent.model, sampling: {} }, (r: any) => { if (r.messages) sent = r.messages; });
  expect(sent[0]!.content.filter((c: any) => c.type === "image").length).toBe(12);
});

test("plugin: models limits it to the models named", async () => {
  const h = await loadPlugin("image-slim", plugin, root, { models: ["other"] });
  const shot = png(1500, 950);
  let changed = false;
  await h.agent.hooks.run("tool_result", { call: { type: "tool_call", id: "1", name: "read", input: {} }, result: { content: [shot] }, ran: true } as any, (r: any) => { if (r.result) changed = true; });
  expect(changed).toBe(false);
});
