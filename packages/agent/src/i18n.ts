/**
 * The language the user interface speaks: one language everywhere, chosen by the user.
 *
 * Strings are written in one language at the call site and carried to the other by the tables in
 * messages-en.ts / messages-ja.ts (gettext style): a string nobody has translated is shown as
 * written, and no call site is blocked on a translation existing. Model-facing text (tool results,
 * prompts) is not translated: it belongs to the conversation, not to the interface.
 */
import { TO_EN } from "./messages-en.ts";
import { TO_JA } from "./messages-ja.ts";

export type Lang = "ja" | "en";

let lang: Lang = "ja";

/** SASACODE_LANG, then the `lang` setting, then the system locale; "ja" when nothing says. */
export function resolveLang(setting?: string): Lang {
  const env = (process.env.SASACODE_LANG ?? "").toLowerCase();
  if (env === "ja" || env === "en") return env;
  if (setting === "ja" || setting === "en") return setting;
  const locale = process.env.LC_ALL ?? process.env.LC_MESSAGES ?? process.env.LANG ?? "";
  if (/^ja/i.test(locale)) return "ja";
  if (!locale || /^(c|posix)(\.|$)/i.test(locale)) return "ja"; // nothing said: the product's own language
  return "en";
}

/** Set once at startup (before the first message is shown). */
export function setLang(next: Lang): void {
  lang = next;
}

export function currentLang(): Lang {
  return lang;
}

/**
 * The text in the current language, with `{name}` placeholders filled from `params`. Call sites
 * write dynamic parts as placeholders (`t("エラー: {error}", { error })`), so the whole sentence
 * is what the translation table matches on.
 */
export function t(text: string, params: Record<string, string | number> = {}): string {
  const out = (lang === "ja" ? TO_JA[text] : TO_EN[text]) ?? text;
  return Object.keys(params).length ? out.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m)) : out;
}
