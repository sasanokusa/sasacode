import type { JSONSchema } from "@sasacode/ai";

/** Shared JSON Schema subset for tool arguments and declared plugin settings. No coercion. */
export function validate(schema: JSONSchema, value: unknown, path = "input"): string[] {
  const errors: string[] = [];
  const type = schema.type as string | string[] | undefined;
  if (type) {
    const types = Array.isArray(type) ? type : [type];
    if (!types.some((t) => typeMatches(t, value))) {
      errors.push(`${path} must be ${types.join(" or ")}, got ${describe(value)}`);
      return errors;
    }
  }
  const equal = (a: unknown, b: unknown): boolean => {
    if (a === b) return true;
    if (!a || !b || typeof a !== "object" || typeof b !== "object" || Array.isArray(a) !== Array.isArray(b)) return false;
    const keys = Object.keys(a);
    return keys.length === Object.keys(b).length && keys.every((k) => Object.hasOwn(b, k) && equal((a as any)[k], (b as any)[k]));
  };
  if (Object.hasOwn(schema, "const") && !equal(schema.const, value)) errors.push(`${path} must match const`);
  for (const keyword of ["anyOf", "oneOf", "allOf"] as const) {
    const branches = schema[keyword];
    if (!Array.isArray(branches)) continue;
    const matches = branches.filter((s) => validate(s as JSONSchema, value, path).length === 0).length;
    if ((keyword === "anyOf" && matches === 0) || (keyword === "oneOf" && matches !== 1) || (keyword === "allOf" && matches !== branches.length))
      errors.push(`${path} must match ${keyword}`);
  }
  if (Array.isArray(schema.enum) && !schema.enum.some((v) => equal(v, value)))
    errors.push(`${path} must be one of ${JSON.stringify(schema.enum)}`);
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const obj = value as Record<string, unknown>;
    const props = (schema.properties ?? {}) as Record<string, JSONSchema>;
    for (const r of (schema.required as string[] | undefined) ?? [])
      if (!Object.hasOwn(obj, r) || obj[r] === undefined) errors.push(`${path}.${r} is required`);
    for (const [k, v] of Object.entries(obj)) {
      if (Object.hasOwn(props, k)) errors.push(...validate(props[k], v, `${path}.${k}`));
      else if (schema.additionalProperties === false) errors.push(`${path}.${k} is not an allowed property`);
      else if (schema.additionalProperties && typeof schema.additionalProperties === "object")
        errors.push(...validate(schema.additionalProperties as JSONSchema, v, `${path}.${k}`));
    }
  }
  if (Array.isArray(value) && schema.items)
    value.forEach((v, i) => errors.push(...validate(schema.items as JSONSchema, v, `${path}[${i}]`)));
  const bound = (key: string, actual: number, smaller: boolean) => {
    const limit = schema[key];
    if (typeof limit === "number" && (smaller ? actual < limit : actual > limit)) errors.push(`${path} violates ${key} ${limit}`);
  };
  if (typeof value === "number") {
    if (!Number.isFinite(value)) errors.push(`${path} must be finite`);
    bound("minimum", value, true); bound("maximum", value, false);
    if (typeof schema.exclusiveMinimum === "number" && value <= schema.exclusiveMinimum) errors.push(`${path} violates exclusiveMinimum`);
    if (typeof schema.exclusiveMaximum === "number" && value >= schema.exclusiveMaximum) errors.push(`${path} violates exclusiveMaximum`);
  }
  if (typeof value === "string") {
    bound("minLength", [...value].length, true); bound("maxLength", [...value].length, false);
    if (typeof schema.pattern === "string") {
      try { if (!new RegExp(schema.pattern, "u").test(value)) errors.push(`${path} does not match pattern`); }
      catch { errors.push(`${path} has an invalid schema pattern`); }
    }
  }
  if (Array.isArray(value)) { bound("minItems", value.length, true); bound("maxItems", value.length, false); }
  else if (value && typeof value === "object") { bound("minProperties", Object.keys(value).length, true); bound("maxProperties", Object.keys(value).length, false); }
  return errors;
}

export const isObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

const TYPE_CHECKS: Record<string, (v: unknown) => boolean> = {
  string: (v) => typeof v === "string",
  number: (v) => typeof v === "number",
  integer: Number.isInteger,
  boolean: (v) => typeof v === "boolean",
  array: Array.isArray,
  object: isObject,
  null: (v) => v === null,
};
const typeMatches = (t: string, v: unknown) => TYPE_CHECKS[t]?.(v) ?? true;

function describe(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
}
