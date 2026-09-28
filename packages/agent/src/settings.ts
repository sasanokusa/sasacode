import type { JSONSchema, SettingsDefinition } from "@sasacode/plugin-api";
import { validate } from "./validate.ts";

const ANNOTATIONS = new Set(["title", "description", "$comment", "examples"]);
const TYPES = ["object", "array", "string", "number", "integer", "boolean", "null"];

/** A deliberately bounded JSON Schema subset; never silently ignore a misspelled constraint. */
function checkSchema(raw: unknown, path = "schema", depth = 0): asserts raw is JSONSchema {
  if (depth > 32) throw new Error(`${path}: schema nesting exceeds 32`);
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error(`${path} must be a schema object`);
  const schema = raw as JSONSchema;
  const bad = (key: string) => { throw new Error(`${path}.${key}: invalid schema constraint`); };
  for (const [key, value] of Object.entries(schema)) {
    if (ANNOTATIONS.has(key)) continue;
    if (key === "type") {
      const values = Array.isArray(value) ? value : [value];
      if (!values.length || values.some((v) => !TYPES.includes(v as string))) bad(key);
    } else if (key === "properties") {
      if (!isObject(value)) bad(key);
      for (const [name, sub] of Object.entries(value as object)) checkSchema(sub, `${path}.properties.${name}`, depth + 1);
    } else if (key === "additionalProperties") {
      if (typeof value !== "boolean") checkSchema(value, `${path}.${key}`, depth + 1);
    } else if (key === "items") checkSchema(value, `${path}.items`, depth + 1);
    else if (["anyOf", "oneOf", "allOf"].includes(key)) {
      if (!Array.isArray(value) || !value.length) bad(key);
      (value as unknown[]).forEach((s, i) => checkSchema(s, `${path}.${key}[${i}]`, depth + 1));
    } else if (key === "required") {
      if (!Array.isArray(value) || value.some((v) => typeof v !== "string") || new Set(value).size !== value.length) bad(key);
    } else if (key === "enum") {
      if (!Array.isArray(value) || !value.length) bad(key);
    } else if (key === "const") { /* any JSON value */ }
    else if (["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"].includes(key)) {
      if (typeof value !== "number" || !Number.isFinite(value)) bad(key);
    } else if (["minLength", "maxLength", "minItems", "maxItems", "minProperties", "maxProperties"].includes(key)) {
      if (!Number.isInteger(value) || (value as number) < 0) bad(key);
    } else if (key === "pattern") {
      if (typeof value !== "string") bad(key);
      try { new RegExp(value as string, "u"); } catch { bad(key); }
    } else throw new Error(`${path}.${key}: unsupported schema keyword`);
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function merge(defaults: unknown, value: unknown, depth = 0): unknown {
  if (depth > 64) throw new Error("settings nesting exceeds 64");
  if (!isObject(defaults) || !isObject(value)) return structuredClone(value);
  // Own data properties, including '__proto__', never setters on Object.prototype.
  return Object.fromEntries([...new Set([...Object.keys(defaults), ...Object.keys(value)])].map((key) => [key,
    Object.hasOwn(value, key) ? Object.hasOwn(defaults, key) ? merge(defaults[key], value[key], depth + 1) : structuredClone(value[key])
      : structuredClone(defaults[key]),
  ]));
}

export function defineSettings<T extends Record<string, unknown>>(name: string, settings: Record<string, unknown>, definition: SettingsDefinition<T>): T {
  try {
    checkSchema(definition.schema);
    if (definition.schema.type !== "object") throw new Error("schema.type must be object");
    if (definition.defaults !== undefined && !isObject(definition.defaults)) throw new Error("defaults must be an object");
    const merged = merge(definition.defaults ?? {}, settings) as T;
    const errors = validate(definition.schema, merged, `plugins.settings.${name}`);
    if (errors.length) throw new Error(errors.join("; "));
    return merged;
  } catch (e) {
    // Diagnostics contain field paths and constraints, never the potentially secret actual values.
    throw new Error(`Plugin ${name} settings: ${e instanceof Error ? e.message : String(e)}`);
  }
}
