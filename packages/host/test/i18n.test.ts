import { afterEach, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { resolveLang, setLang, t } from "../src/i18n.ts";
import { TO_EN } from "../src/messages-en.ts";
import { TO_JA } from "../src/messages-ja.ts";

afterEach(() => setLang("ja"));

const JA = /[ぁ-んァ-ヶ一-龠]/;
const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

/** Every t("…") / t('…') literal in the interface's sources. */
function literals(): { file: string; text: string }[] {
  const root = join(import.meta.dir, "../..");
  const out: { file: string; text: string }[] = [];
  for (const pkg of ["tui", "cli", "bundled", "agent", "host"]) {
    const dir = join(root, pkg, "src");
    for (const f of readdirSync(dir, { recursive: true }).map(String).filter((f) => f.endsWith(".ts") && !f.startsWith("messages-"))) {
      const src = readFileSync(join(dir, f), "utf8");
      // Constants shown through t(NAME): `const NAME = "…"` or a template without substitutions.
      for (const m of src.matchAll(/\bt\(([A-Z_a-z]\w*)[,)]/g)) {
        const def = new RegExp(`const ${m[1]} = (\`[^\`$]*\`|"(?:[^"\\\\]|\\\\.)*")`).exec(src)?.[1];
        if (def) out.push({ file: `${pkg}/${f}`, text: def.startsWith("`") ? def.slice(1, -1) : JSON.parse(def) });
      }
      // Tables of messages looked up and then translated (headless.ts's STOP_HINT).
      for (const m of src.matchAll(/const (\w+): Partial<Record<\w+, string>> = \{([^}]*)\}/g))
        for (const v of m[2]!.matchAll(/:\s*("(?:[^"\\]|\\.)*")/g)) out.push({ file: `${pkg}/${f}`, text: JSON.parse(v[1]!) });
      for (const m of src.matchAll(/\bt\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')/g)) {
        const raw = m[1]!;
        const text = raw.startsWith('"') ? JSON.parse(raw) : JSON.parse(`"${raw.slice(1, -1).replace(/\\'/g, "'").replace(/"/g, '\\"')}"`);
        out.push({ file: `${pkg}/${f}`, text });
      }
    }
  }
  return out;
}

test("every string shown through t() has a translation, with the same placeholders", () => {
  const found = literals();
  expect(found.length).toBeGreaterThan(100);
  const missing = found.filter(({ text }) => !(JA.test(text) ? TO_EN : TO_JA)[text]).map(({ file, text }) => `${file}: ${text}`);
  expect(missing).toEqual([]);
  for (const table of [TO_EN, TO_JA])
    for (const [from, to] of Object.entries(table)) expect(`${from} → ${placeholders(to)}`).toBe(`${from} → ${placeholders(from)}`);
});

test("t() follows the language and fills placeholders", () => {
  expect(t("エラー: {error}", { error: "x" })).toBe("エラー: x");
  setLang("en");
  expect(t("エラー: {error}", { error: "x" })).toBe("Error: x");
  expect(t("not in any table")).toBe("not in any table");
});

test("resolveLang: SASACODE_LANG, then the setting, then the locale", () => {
  const saved = { ...process.env };
  try {
    delete process.env.LC_ALL;
    delete process.env.LC_MESSAGES;
    process.env.SASACODE_LANG = "en";
    expect(resolveLang("ja")).toBe("en");
    delete process.env.SASACODE_LANG;
    expect(resolveLang("en")).toBe("en");
    process.env.LANG = "en_US.UTF-8";
    expect(resolveLang()).toBe("en");
    process.env.LANG = "ja_JP.UTF-8";
    expect(resolveLang()).toBe("ja");
  } finally {
    // Restored in place: a plain object in place of process.env would lose Windows' case-insensitive
    // names (Path is not PATH), and children started later would get no PATH.
    for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k];
    Object.assign(process.env, saved);
  }
});
