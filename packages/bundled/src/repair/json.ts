// Lenient JSON for tool arguments written by small models. One pass over the text that knows
// whether it is inside a string, so fixes never touch string contents.

export interface Repaired<T> {
  value: T;
  fixes: string[];
}

const PY_LITERALS: Record<string, string> = { True: "true", False: "false", None: "null", undefined: "null" };

/** Repair and parse `raw` as a JSON object. Undefined when it cannot be made into one. */
export function repairJson(raw: string): Repaired<Record<string, unknown>> | undefined {
  const fixes = new Set<string>();
  let src = raw.trim();

  // Arguments sometimes arrive as a JSON string holding the JSON object.
  const direct = parse(src);
  if (typeof direct === "string") {
    const inner = repairJson(direct);
    return inner && { value: inner.value, fixes: ["decoded double-encoded JSON", ...inner.fixes] };
  }
  if (isObject(direct)) return { value: direct, fixes: [] };

  const fence = /^```[\w-]*\s*([\s\S]*?)\s*```$/.exec(src);
  if (fence) {
    src = fence[1]!;
    fixes.add("removed code fence");
  }
  const start = src.search(/[{[]/);
  if (start < 0) return undefined;
  if (start > 0) {
    src = src.slice(start);
    fixes.add("dropped text before the JSON");
  }

  const out: string[] = [];
  const stack: string[] = [];
  let quote: '"' | "'" | undefined;
  let escaped = false;
  let end = src.length;

  const dropTrailingComma = () => {
    for (let j = out.length - 1; j >= 0; j--) {
      if (/\s/.test(out[j]!)) continue;
      if (out[j] === ",") {
        out.splice(j, 1);
        fixes.add("removed trailing comma");
      }
      return;
    }
  };

  for (let i = 0; i < src.length; i++) {
    const ch = src[i]!;
    if (quote) {
      if (escaped) {
        escaped = false;
        // \' is not a JSON escape; inside a converted single-quoted string it is just a quote.
        out.push(ch === "'" && quote === "'" ? "'" : `\\${ch}`);
      } else if (ch === "\\") escaped = true;
      else if (ch === quote) {
        quote = undefined;
        out.push('"');
      } else if (ch === '"') out.push('\\"');
      else if (ch === "\n" || ch === "\r" || ch === "\t") {
        out.push(ch === "\n" ? "\\n" : ch === "\r" ? "\\r" : "\\t");
        fixes.add("escaped control characters in strings");
      } else out.push(ch);
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      if (ch === "'") fixes.add("converted single quotes");
      out.push('"');
    } else if (ch === "{" || ch === "[") {
      stack.push(ch === "{" ? "}" : "]");
      out.push(ch);
    } else if (ch === "}" || ch === "]") {
      dropTrailingComma();
      stack.pop();
      out.push(ch);
      if (!stack.length) {
        end = i + 1;
        break;
      }
    } else if (/[A-Za-z_$]/.test(ch)) {
      const word = /^[A-Za-z_$][\w$-]*/.exec(src.slice(i))![0];
      const after = src.slice(i + word.length).trimStart();
      if (after.startsWith(":")) {
        out.push(JSON.stringify(word));
        fixes.add("quoted keys");
      } else if (PY_LITERALS[word]) {
        out.push(PY_LITERALS[word]!);
        fixes.add(`${word} → ${PY_LITERALS[word]}`);
      } else out.push(word);
      i += word.length - 1;
    } else out.push(ch);
  }

  if (end < src.length && src.slice(end).trim()) fixes.add("dropped text after the JSON");
  if (quote) {
    out.push('"');
    fixes.add("closed an unterminated string");
  }
  if (stack.length) {
    dropTrailingComma();
    out.push(...stack.reverse());
    fixes.add("closed brackets");
  }

  const value = parse(out.join(""));
  return isObject(value) ? { value, fixes: [...fixes] } : undefined;
}

function parse(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return undefined;
  }
}

function isObject(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}
