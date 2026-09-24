import type { JSONSchema } from "@sasacode/ai";

/** Minimal JSON Schema check for tool arguments: types, required, enum, additionalProperties. */
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
  if (Array.isArray(schema.enum) && !schema.enum.includes(value))
    errors.push(`${path} must be one of ${JSON.stringify(schema.enum)}`);
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const obj = value as Record<string, unknown>;
    const props = (schema.properties ?? {}) as Record<string, JSONSchema>;
    for (const r of (schema.required as string[] | undefined) ?? [])
      if (obj[r] === undefined) errors.push(`${path}.${r} is required`);
    for (const [k, v] of Object.entries(obj)) {
      if (props[k]) errors.push(...validate(props[k], v, `${path}.${k}`));
      else if (schema.additionalProperties === false) errors.push(`${path}.${k} is not an allowed property`);
    }
  }
  if (Array.isArray(value) && schema.items)
    value.forEach((v, i) => errors.push(...validate(schema.items as JSONSchema, v, `${path}[${i}]`)));
  return errors;
}

function typeMatches(t: string, v: unknown): boolean {
  switch (t) {
    case "string":
      return typeof v === "string";
    case "number":
      return typeof v === "number";
    case "integer":
      return Number.isInteger(v);
    case "boolean":
      return typeof v === "boolean";
    case "array":
      return Array.isArray(v);
    case "object":
      return !!v && typeof v === "object" && !Array.isArray(v);
    case "null":
      return v === null;
    default:
      return true;
  }
}

function describe(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
}
