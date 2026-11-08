import { createHash } from "node:crypto";
import { inflateSync } from "node:zlib";
import { t } from "@sasacode/host";
import type { Plugin, UserContent } from "@sasacode/plugin-api";

type Image = Extract<UserContent, { type: "image" }>;
type Shrunk = { mediaType: string; data: string; note: string };
type Probe = { kind: "png" | "jpeg"; w: number; h: number; alpha: boolean };

/**
 * Every request carries every image in the history, so a session that reads many screenshots grows
 * past a gateway's body limit (413 at 8 MiB on llm.sasanokusa.com after 20 of them) and pays for
 * them all each time.
 *
 * 1. Shrink, keeping what the model sees: providers scale large images down anyway (Qwen's encoder
 *    to about 1,048,576 px, Anthropic's to about 1.15 MP), so an image over `maxPixels` is resized
 *    to that area, aspect ratio kept (measured on Qwen3.8 Flash Next: the same prompt_tokens and
 *    the same small text read back; a long side of 1024 px read less). A PNG without transparency
 *    over `minBytes` becomes a JPEG (`jpegQuality`; 80 to 95 read the same text as the PNG). The
 *    original stays when the copy is not smaller. Done once when a tool returns an image (the
 *    session keeps the copy), and in each request for images that came another way (pasted, older
 *    sessions). Bun.Image does the work, on every OS.
 * 2. Leave out old images: only the newest go with a request, older ones become a short note in it
 *    (the session keeps them). The cut moves `dropStep` images at a time, so between steps the start
 *    of the prompt stays the same and the provider's cache holds. Images in the latest user message
 *    always go; past `maxImageBytes` of images, more old ones are left out.
 *
 * Command: /image-slim (what the last request sent).
 * settings: { models (["*"]), maxPixels (1048576), jpegQuality (90), minBytes (100000),
 *   keepImages (4) and dropStep (8): 4 to 11 images go, maxImageBytes (8 MiB of base64) }
 */

const OMITTED =
  "[An image was here. It is left out of this request to keep the request small. Read the file again (or ask the user to paste it again) if you need to see it.]";

/** Width, height and transparency from the header, without decoding the image. */
export function probe(buf: Uint8Array): Probe | undefined {
  const b = Buffer.from(buf.buffer, buf.byteOffset, buf.byteLength);
  if (b.length > 33 && b.readUInt32BE(0) === 0x89504e47 && b.toString("latin1", 12, 16) === "IHDR") {
    let alpha = b[25] === 4 || b[25] === 6;
    // A tRNS chunk before the image data also means transparency.
    for (let i = 8; !alpha && i + 8 <= b.length; ) {
      const type = b.toString("latin1", i + 4, i + 8);
      if (type === "tRNS") alpha = true;
      if (type === "IDAT" || type === "IEND") break;
      i += 12 + b.readUInt32BE(i);
    }
    return { kind: "png", w: b.readUInt32BE(16), h: b.readUInt32BE(20), alpha };
  }
  if (b.length > 4 && b[0] === 0xff && b[1] === 0xd8) {
    for (let i = 2; i + 9 < b.length; ) {
      const m = b[i + 1]!;
      if (b[i] !== 0xff || m === 0xff) { i++; continue; }
      if (m === 0x01 || (m >= 0xd0 && m <= 0xd8)) { i += 2; continue; }
      // A start-of-frame marker (not DHT, JPG, DAC) holds the size.
      if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8 && m !== 0xcc) return { kind: "jpeg", w: b.readUInt16BE(i + 7), h: b.readUInt16BE(i + 5), alpha: false };
      i += 2 + b.readUInt16BE(i + 2);
    }
  }
  return undefined;
}

/**
 * Whether a PNG whose format has an alpha channel (8-bit gray or RGB with alpha, not interlaced) uses
 * it nowhere: macOS saves every screenshot so, and such a file can go as a JPEG.
 */
export function opaque(b: Buffer): boolean {
  if (b[24] !== 8 || (b[25] !== 4 && b[25] !== 6) || b[28] !== 0) return false;
  const idat: Buffer[] = [];
  for (let i = 8; i + 8 <= b.length; i += 12 + b.readUInt32BE(i)) {
    const type = b.toString("latin1", i + 4, i + 8);
    if (type === "tRNS") return false;
    if (type === "IDAT") idat.push(b.subarray(i + 8, i + 8 + b.readUInt32BE(i)));
  }
  let data: Buffer;
  try {
    data = inflateSync(Buffer.concat(idat));
  } catch {
    return false;
  }
  const bpp = b[25] === 6 ? 4 : 2, row = b.readUInt32BE(16) * bpp, h = b.readUInt32BE(20);
  if (data.length < (row + 1) * h) return false;
  let prev = new Uint8Array(row), cur = new Uint8Array(row);
  // Undo each row's filter (PNG spec 9.2), then look at every pixel's last byte.
  for (let y = 0; y < h; y++) {
    const at = y * (row + 1) + 1, f = data[at - 1];
    for (let i = 0; i < row; i++) {
      const x = data[at + i]!, a = i >= bpp ? cur[i - bpp]! : 0, up = prev[i]!, c = i >= bpp ? prev[i - bpp]! : 0;
      if (f === 0) cur[i] = x;
      else if (f === 1) cur[i] = x + a;
      else if (f === 2) cur[i] = x + up;
      else if (f === 3) cur[i] = x + ((a + up) >> 1);
      else if (f === 4) {
        const pa = Math.abs(up - c), pb = Math.abs(a - c), pc = Math.abs(a + up - 2 * c);
        cur[i] = x + (pa <= pb && pa <= pc ? a : pb <= pc ? up : c);
      } else return false;
    }
    for (let i = bpp - 1; i < row; i += bpp) if (cur[i] !== 255) return false;
    [prev, cur] = [cur, prev];
  }
  return true;
}

const kb = (base64: number) => `${Math.round((base64 * 3) / 4 / 1024)} KB`;

export type SlimSettings = {
  models: string[];
  maxPixels: number;
  jpegQuality: number;
  minBytes: number;
  keepImages: number;
  dropStep: number;
  maxImageBytes: number;
};

/** A smaller copy of the image, or undefined to send it as it is. */
export async function shrink(img: Image, s: Pick<SlimSettings, "maxPixels" | "jpegQuality" | "minBytes">): Promise<Shrunk | undefined> {
  // The header (and a JPEG's frame after its EXIF) is near the start: decode only that much to decide.
  const p = probe(Buffer.from(img.data.slice(0, 349_528), "base64"));
  if (!p?.w || !p.h) return;
  const big = p.w * p.h > s.maxPixels;
  if (!big && (p.kind === "jpeg" || (img.data.length * 3) / 4 < s.minBytes)) return;
  const original = Buffer.from(img.data, "base64");
  const alpha = p.alpha && !opaque(original);
  if (!big && alpha) return;
  const jpeg = !alpha;
  let w = p.w, h = p.h;
  let pipe = new Bun.Image(original);
  if (big) {
    const scale = Math.sqrt(s.maxPixels / (p.w * p.h));
    w = Math.max(1, Math.floor(p.w * scale));
    h = Math.max(1, Math.floor(p.h * scale));
    pipe = pipe.resize(w, h);
  }
  let out: Uint8Array;
  try {
    out = await (jpeg ? pipe.jpeg({ quality: s.jpegQuality }) : pipe.png()).bytes();
  } catch {
    return; // a damaged or unusual file goes as it is
  }
  if (out.length >= original.length) return;
  const format = jpeg ? `JPEG q${s.jpegQuality}` : "PNG";
  const scaled = big ? `; multiply positions in it by ${(p.w / w).toFixed(2)} for the original` : "";
  return {
    mediaType: jpeg ? "image/jpeg" : "image/png",
    data: Buffer.from(out).toString("base64"),
    note: `${p.w}x${p.h} ${p.kind.toUpperCase()} ${kb(img.data.length)} -> ${w}x${h} ${format} ${kb((out.length * 4) / 3)}${scaled}`,
  };
}

/** Which images (`message:block`) to leave out: the oldest, `dropStep` at a time, never the latest user message's. */
export function omit(spots: { key: string; bytes: number; locked: boolean }[], s: Pick<SlimSettings, "keepImages" | "dropStep" | "maxImageBytes">): Set<string> {
  const droppable = spots.filter((sp) => !sp.locked);
  const over = spots.length - s.keepImages;
  let drop = Math.min(over > 0 ? Math.floor(over / s.dropStep) * s.dropStep : 0, droppable.length);
  let bytes = spots.reduce((n, sp) => n + sp.bytes, 0) - droppable.slice(0, drop).reduce((n, sp) => n + sp.bytes, 0);
  while (bytes > s.maxImageBytes && drop < droppable.length) bytes -= droppable[drop++]!.bytes;
  return new Set(droppable.slice(0, drop).map((sp) => sp.key));
}

const imageSlim: Plugin = (api) => {
  const s = api.defineSettings<SlimSettings>({
    schema: {
      type: "object",
      properties: {
        models: { type: "array", items: { type: "string", minLength: 1 } },
        maxPixels: { type: "integer", minimum: 65536 },
        jpegQuality: { type: "integer", minimum: 50, maximum: 100 },
        minBytes: { type: "integer", minimum: 0 },
        keepImages: { type: "integer", minimum: 1 },
        dropStep: { type: "integer", minimum: 1 },
        maxImageBytes: { type: "integer", minimum: 1 },
      },
      additionalProperties: false,
    },
    defaults: { models: ["*"], maxPixels: 1_048_576, jpegQuality: 90, minBytes: 100_000, keepImages: 4, dropStep: 8, maxImageBytes: 8 * 1024 * 1024 },
  });
  const targets = (id: string | undefined) => !!id && (s.models.includes("*") || s.models.includes(id));

  // Each request shrinks the same images again: keep the answer by content.
  const cache = new Map<string, Shrunk | null>();
  async function cached(img: Image): Promise<Shrunk | undefined> {
    const key = createHash("sha1").update(img.data).digest("hex");
    if (!cache.has(key)) {
      cache.set(key, (await shrink(img, s)) ?? null);
      if (cache.size > 256) cache.delete(cache.keys().next().value!);
    }
    return cache.get(key) ?? undefined;
  }

  let last: { total: number; sent: number; bytes: number } | undefined;
  const stored = { count: 0, before: 0, after: 0 };

  api.on("tool_result", async ({ result }) => {
    if (!targets(api.agent.model()?.id) || !result.content.some((c) => c.type === "image")) return;
    const content: typeof result.content = [];
    let changed = false;
    for (const c of result.content) {
      const r = c.type === "image" ? await cached(c) : undefined;
      if (c.type !== "image" || !r) { content.push(c); continue; }
      changed = true;
      stored.count++;
      stored.before += c.data.length;
      stored.after += r.data.length;
      content.push({ type: "image", mediaType: r.mediaType, data: r.data }, { type: "text", text: `[image-slim: stored as a smaller copy, ${r.note}]` });
    }
    if (changed) return { result: { ...result, content } };
  });

  api.on("before_request", async ({ messages, model }) => {
    if (!targets(model.id)) return;
    const lastUser = messages.findLastIndex((m) => m.role === "user");
    const spots: { key: string; bytes: number; locked: boolean }[] = [];
    const replaced = new Map<string, Image>();
    for (const [mi, m] of messages.entries()) {
      if (m.role === "assistant") continue;
      for (const [ci, c] of m.content.entries()) {
        if (c.type !== "image") continue;
        const key = `${mi}:${ci}`;
        const r = await cached(c);
        if (r) replaced.set(key, { type: "image", mediaType: r.mediaType, data: r.data });
        spots.push({ key, bytes: (r ?? c).data.length, locked: mi === lastUser });
      }
    }
    if (!spots.length) return void (last = undefined);
    const omitted = omit(spots, s);
    last = { total: spots.length, sent: spots.length - omitted.size, bytes: spots.reduce((n, sp) => n + (omitted.has(sp.key) ? 0 : sp.bytes), 0) };
    if (!replaced.size && !omitted.size) return;
    const next = messages.map((m, mi) => {
      if (m.role === "assistant" || !m.content.some((_, ci) => replaced.has(`${mi}:${ci}`) || omitted.has(`${mi}:${ci}`))) return m;
      const content = m.content.map((c, ci) => (omitted.has(`${mi}:${ci}`) ? { type: "text" as const, text: OMITTED } : (replaced.get(`${mi}:${ci}`) ?? c)));
      return { ...m, content } as typeof m;
    });
    return { messages: next };
  });

  api.registerCommand({
    name: "image-slim",
    description: t("画像を縮め、古い画像を送らない処理の状態を出す"),
    complete: () => [],
    run: () => {
      const id = api.agent.model()?.id ?? "?";
      const lines = [targets(id) ? t("いまのモデル（{id}）に効きます。", { id }) : t("いまのモデル（{id}）には効きません（対象: {models}）。", { id, models: s.models.join(", ") })];
      if (last) lines.push(t("直近のリクエスト: 画像{total}枚のうち{sent}枚を送り、{omitted}枚は省きました（送った画像は計{size}）。", { total: last.total, sent: last.sent, omitted: last.total - last.sent, size: kb(last.bytes) }));
      if (stored.count) lines.push(t("読んだ画像を縮めた数: {count}枚（{before} → {after}）", { count: stored.count, before: kb(stored.before), after: kb(stored.after) }));
      api.ui.notify(lines.join("\n"));
    },
  });
};
export default imageSlim;
