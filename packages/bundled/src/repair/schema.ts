// Bring parsed arguments in line with a tool's JSON Schema, but only where the intent is unambiguous:
// one candidate key, one obvious type conversion. Anything else is left for validation to report.
import type { JSONSchema } from "@sasacode/plugin-api";
import type { Repaired } from "./json.ts";

/** Common alternative names small models use for well-known argument names. */
const SYNONYMS: Record<string, string[]> = {
  path: ["file", "filepath", "filename", "file_name", "filePath", "file_path", "target", "target_file"],
  command: ["cmd", "shell", "script", "bash", "commandLine"],
  old_string: ["old", "old_str", "oldText", "old_text", "search", "find", "from", "original"],
  new_string: ["new", "new_str", "newText", "new_text", "replace", "replacement", "to"],
  content: ["contents", "text", "data", "body", "file_content", "code"],
  query: ["q", "search", "keywords", "search_query", "term"],
  url: ["link", "uri", "href", "address"],
};

const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");

/** Edit distance where swapping two neighbouring characters counts as one edit (raed → read). */
export function editDistance(a: string, b: string): number {
  const d = Array.from({ length: a.length + 1 }, (_, i) => Array.from({ length: b.length + 1 }, (_, j) => (i === 0 ? j : j === 0 ? i : 0)));
  for (let i = 1; i <= a.length; i++)
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      d[i]![j] = Math.min(d[i - 1]![j]! + 1, d[i]![j - 1]! + 1, d[i - 1]![j - 1]! + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) d[i]![j] = Math.min(d[i]![j]!, d[i - 2]![j - 2]! + 1);
    }
  return d[a.length]![b.length]!;
}

/** The single best match for `name` among `candidates`, or undefined when none or several fit. */
export function closestName(name: string, candidates: string[], synonyms: Record<string, string[]> = {}): string | undefined {
  const n = norm(name);
  const pick = (hits: string[]) => (hits.length === 1 ? hits[0] : undefined);
  const exact = candidates.filter((c) => norm(c) === n);
  if (exact.length) return pick(exact);
  const syn = candidates.filter((c) => synonyms[c]?.some((s) => norm(s) === n));
  if (syn.length) return pick(syn);
  const limit = n.length < 5 ? 1 : 2;
  const scored = candidates.map((c) => ({ c, d: editDistance(n, norm(c)) })).filter((x) => x.d <= limit);
  if (!scored.length) return undefined;
  const best = Math.min(...scored.map((x) => x.d));
  return pick(scored.filter((x) => x.d === best).map((x) => x.c));
}

function typesOf(schema: JSONSchema): string[] {
  const t = schema.type;
  return Array.isArray(t) ? (t as string[]) : typeof t === "string" ? [t] : [];
}

function matches(type: string, v: unknown): boolean {
  if (type === "integer") return Number.isInteger(v);
  if (type === "array") return Array.isArray(v);
  if (type === "object") return !!v && typeof v === "object" && !Array.isArray(v);
  if (type === "null") return v === null;
  return typeof v === type;
}

function parseJson(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return undefined;
  }
}

/** Convert one value toward the schema's type. Returns the value unchanged when no safe conversion exists. */
function convert(v: unknown, schema: JSONSchema, at: string, fixes: string[]): unknown {
  const types = typesOf(schema);
  if (Array.isArray(schema.enum) && typeof v === "string" && !schema.enum.includes(v)) {
    const hit = (schema.enum as unknown[]).filter((e) => typeof e === "string" && e.toLowerCase() === v.toLowerCase());
    if (hit.length === 1) {
      fixes.push(`${at}: "${v}" → "${hit[0]}"`);
      return hit[0];
    }
  }
  if (!types.length || types.some((t) => matches(t, v))) {
    return types.includes("object") || types.includes("array") ? recurse(v, schema, at, fixes) : v;
  }
  const show = (x: unknown) => JSON.stringify(x);
  for (const t of types) {
    let out: unknown;
    if ((t === "number" || t === "integer") && typeof v === "string" && /^\s*-?\d+(\.\d+)?\s*$/.test(v)) out = Number(v);
    else if (t === "boolean" && typeof v === "string" && /^(true|false)$/i.test(v.trim())) out = v.trim().toLowerCase() === "true";
    else if (t === "string" && (typeof v === "number" || typeof v === "boolean")) out = String(v);
    else if ((t === "object" || t === "array") && typeof v === "string") out = parseJson(v);
    else if (t === "array" && v !== null && v !== undefined) out = [v];
    if (out !== undefined && matches(t, out)) {
      fixes.push(`${at}: ${show(v).slice(0, 40)} → ${show(out).slice(0, 40)}`);
      return recurse(out, schema, at, fixes);
    }
  }
  return v;
}

function recurse(v: unknown, schema: JSONSchema, at: string, fixes: string[]): unknown {
  if (Array.isArray(v) && schema.items) return v.map((x, i) => convert(x, schema.items as JSONSchema, `${at}[${i}]`, fixes));
  if (v && typeof v === "object" && !Array.isArray(v) && schema.properties) return fixObject(v as Record<string, unknown>, schema, `${at}.`, fixes);
  return v;
}

function fixObject(input: Record<string, unknown>, schema: JSONSchema, prefix: string, fixes: string[]): Record<string, unknown> {
  const props = (schema.properties ?? {}) as Record<string, JSONSchema>;
  const required = new Set((schema.required as string[] | undefined) ?? []);
  const out: Record<string, unknown> = {};
  const unknown = Object.keys(input).filter((k) => !(k in props));
  for (const k of Object.keys(input)) if (k in props) out[k] = input[k];
  for (const k of unknown) {
    const free = Object.keys(props).filter((p) => !(p in out));
    // Prefer filling a missing required argument over an optional one.
    const target = closestName(k, free.filter((p) => required.has(p)), SYNONYMS) ?? closestName(k, free, SYNONYMS);
    if (target) {
      out[target] = input[k];
      fixes.push(`argument "${prefix}${k}" → "${prefix}${target}"`);
    } else if (schema.additionalProperties === false) {
      fixes.push(`dropped unknown argument "${prefix}${k}"`);
    } else out[k] = input[k];
  }
  for (const [k, v] of Object.entries(out)) {
    if (v === null && !required.has(k) && !typesOf(props[k] ?? {}).includes("null")) {
      delete out[k];
      fixes.push(`dropped null "${prefix}${k}"`);
    } else if (props[k]) out[k] = convert(v, props[k]!, `${prefix}${k}`, fixes);
  }
  return out;
}

/** Rename, convert and drop arguments so they fit `schema`; each change is listed in `fixes`. */
export function fitToSchema(input: Record<string, unknown>, schema: JSONSchema): Repaired<Record<string, unknown>> {
  const fixes: string[] = [];
  return { value: fixObject(input, schema, "", fixes), fixes };
}
