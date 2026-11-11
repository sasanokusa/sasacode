import { homedir } from "node:os";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** Schemes whose URL can be digits after the colon (`tel:5551234`): not a file with a line. */
const DIGIT_SCHEMES = /^(?:tel|sms|fax|callto):/i;

/**
 * Where a markdown link should point so the terminal can open it. A model writes local files as
 * `docs/a.md`, `src/x.ts:42`, `x.ts:42`, `README.md#setup` or `/abs/path#L3`; a terminal only opens
 * URLs, so a path becomes a file:// URL against `cwd` (a line suffix and a `#` heading are dropped:
 * a file URL has no place for a line, and an editor opens it at the top). URLs and in-page anchors
 * are left alone.
 */
export function linkTarget(href: string, cwd: string): string {
  if (!href || href.startsWith("#")) return href;
  const path = href.startsWith("~/") ? homedir() + href.slice(1) : href;
  // `x.ts:42` and `Makefile:12` (a line, maybe a heading after it) are files in the folder, not URLs (#115).
  const fileLine = /^[^:/]+:\d+(?::\d+)?(?:#.*)?$/s.test(path) && !DIGIT_SCHEMES.test(path);
  if (!isAbsolute(path) && !fileLine && /^[a-z][a-z0-9+.-]*:/i.test(path)) return href;
  // The fragment goes before decoding: a `%23` in a name is a real `#` of the file's (#115).
  let p = path.replace(/#.*$/s, "").replace(/:\d+(?::\d+)?$/, "");
  try {
    p = decodeURIComponent(p); // not decodeURI, which keeps %23 (#138)
  } catch {}
  return pathToFileURL(resolve(cwd, p)).href;
}

const LINK = /\]\(\s*(<[^>\n]*>|[^)\s]+)/g;

/** Rewrites the targets of inline links (`[text](target)`) with linkTarget; code is left as written. */
export function localLinks(markdown: string, cwd: string): string {
  if (!markdown.includes("](")) return markdown;
  let fence: string | undefined;
  return markdown
    .split("\n")
    .map((line) => {
      const open = /^\s*(`{3,}|~{3,})/.exec(line)?.[1];
      if (fence) {
        if (open && open[0] === fence[0] && open.length >= fence.length) fence = undefined;
        return line;
      }
      if (open) {
        fence = open;
        return line;
      }
      // Code spans keep their text; only the parts between them are links.
      return line
        .split(/(`+[^`]*`+)/)
        .map((part, i) =>
          i % 2
            ? part
            : part.replace(LINK, (m, target: string) => {
                const angle = target.startsWith("<");
                const to = linkTarget(angle ? target.slice(1, -1) : target, cwd);
                // As a function: `$&` or `$$` in a path is not a replacement pattern (#115).
                return m.replace(target, () => (angle ? `<${to}>` : to));
              }),
        )
        .join("");
    })
    .join("\n");
}
