import { homedir } from "node:os";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/**
 * Where a markdown link should point so the terminal can open it. A model writes local files as
 * `docs/a.md`, `src/x.ts:42` or `/abs/path#L3`; a terminal only opens URLs, so a path becomes a
 * file:// URL against `cwd` (a line suffix is dropped: file URLs have no place for it). URLs and
 * in-page anchors are left alone.
 */
export function linkTarget(href: string, cwd: string): string {
  if (!href || href.startsWith("#")) return href;
  const path = href.startsWith("~/") ? homedir() + href.slice(1) : href;
  if (!isAbsolute(path) && /^[a-z][a-z0-9+.-]*:/i.test(path)) return href;
  let p = path.replace(/#L\d+(?:-L?\d+)?$/, "").replace(/:\d+(?::\d+)?$/, "");
  try {
    p = decodeURI(p);
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
                return m.replace(target, angle ? `<${to}>` : to);
              }),
        )
        .join("");
    })
    .join("\n");
}
