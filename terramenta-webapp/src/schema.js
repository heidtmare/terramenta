/**
 * JSON Schema (2020-12) subset validator, enough for
 * `schema/project.schema.json`.
 *
 * Keywords: `$ref` (local `#/...`), `type`, `const`, `enum`, `properties`,
 * `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minimum`, `maximum`,
 * `exclusiveMinimum`, `exclusiveMaximum`, `pattern`, `format: "date-time"`,
 * `anyOf`, `allOf`, `if`/`then`/`else`. Others are ignored.
 *
 * Deviation: `additionalProperties: false` reports unknown keys to `warn`
 * instead of failing, so files with keys from newer builds still open.
 * A schema's `title` is used as the expectation in its messages.
 */

/**
 * Validates `value`; returns `"path: message"` strings (empty if valid).
 * `warn(path)` receives each unknown key.
 */
export function validate(schema, value, { warn = () => {} } = {}) {
  const errors = [];
  check(schema, value, "", { root: schema, errors, warn });
  return errors;
}

function check(schema, value, path, context) {
  if (schema === true || schema === undefined) return;
  if (schema === false) {
    fail(context, path, "nothing", value);
    return;
  }
  if (schema.$ref !== undefined) check(resolve(context.root, schema.$ref), value, path, context);

  if (schema.type !== undefined) {
    const types = [schema.type].flat();
    if (!types.some((type) => isType(type, value))) {
      fail(context, path, schema.title ?? types.map(article).join(" or "), value);
      return;
    }
  }
  if (schema.const !== undefined && !equal(value, schema.const)) {
    fail(context, path, JSON.stringify(schema.const), value);
    return;
  }
  if (schema.enum !== undefined && !schema.enum.some((option) => equal(value, option))) {
    fail(context, path, `one of ${schema.enum.join(", ")}`, value);
    return;
  }

  if (typeof value === "number") checkNumber(schema, value, path, context);
  if (typeof value === "string") checkString(schema, value, path, context);
  if (isType("object", value)) checkObject(schema, value, path, context);
  if (Array.isArray(value)) checkArray(schema, value, path, context);

  if (schema.anyOf && !schema.anyOf.some((branch) => passes(branch, value, context))) {
    fail(context, path, schema.title ?? schema.anyOf.map((branch) => describe(branch, context)).join(" or "), value);
  }
  for (const branch of schema.allOf ?? []) check(branch, value, path, context);
  if (schema.if !== undefined) {
    const branch = passes(schema.if, value, context) ? schema.then : schema.else;
    check(branch, value, path, context);
  }
}

function checkNumber(schema, value, path, context) {
  const bounds = [
    ["minimum", (limit) => value >= limit, "≥"],
    ["exclusiveMinimum", (limit) => value > limit, ">"],
    ["maximum", (limit) => value <= limit, "≤"],
    ["exclusiveMaximum", (limit) => value < limit, "<"],
  ];
  for (const [keyword, holds, symbol] of bounds) {
    const limit = schema[keyword];
    if (limit !== undefined && !holds(limit)) {
      fail(context, path, `a number ${symbol} ${limit}`, value);
      return;
    }
  }
}

function checkString(schema, value, path, context) {
  if (schema.pattern !== undefined && !new RegExp(schema.pattern, "u").test(value)) {
    fail(context, path, schema.title ?? `a string matching ${schema.pattern}`, value);
  } else if (schema.format === "date-time" && !Number.isFinite(Date.parse(value))) {
    fail(context, path, schema.title ?? "an ISO 8601 date-time", value);
  }
}

function checkArray(schema, value, path, context) {
  const { minItems = 0, maxItems = Infinity } = schema;
  if (value.length < minItems || value.length > maxItems) {
    const count = minItems === maxItems ? `${minItems}` : maxItems === Infinity ? `≥ ${minItems}` : `${minItems}–${maxItems}`;
    fail(context, path, schema.title ?? `${count} items`, value);
    return;
  }
  if (schema.items !== undefined) {
    value.forEach((item, index) => check(schema.items, item, `${path}[${index}]`, context));
  }
}

function checkObject(schema, value, path, context) {
  for (const key of schema.required ?? []) {
    if (value[key] === undefined) context.errors.push(`${join(path, key)}: required`);
  }
  const properties = schema.properties ?? {};
  for (const [key, child] of Object.entries(value)) {
    if (key in properties) check(properties[key], child, join(path, key), context);
    else if (schema.additionalProperties === false) context.warn(join(path, key));
    else check(schema.additionalProperties, child, join(path, key), context);
  }
}

/** Whether `value` matches `schema`, reporting nothing. */
function passes(schema, value, context) {
  const scratch = { ...context, errors: [], warn: () => {} };
  check(schema, value, "", scratch);
  return scratch.errors.length === 0;
}

/** Short expectation for an `anyOf` branch. */
function describe(schema, context) {
  if (schema.$ref !== undefined) return describe(resolve(context.root, schema.$ref), context);
  if (schema.title) return schema.title;
  if (schema.const !== undefined) return JSON.stringify(schema.const);
  if (schema.enum) return `one of ${schema.enum.join(", ")}`;
  if (schema.format === "date-time") return "an ISO 8601 date-time";
  if (schema.type !== undefined) return [schema.type].flat().map(article).join(" or ");
  return "a value";
}

function resolve(root, ref) {
  if (!ref.startsWith("#")) throw new Error(`schema: only local $ref supported, got ${ref}`);
  const target = ref
    .slice(1)
    .split("/")
    .filter(Boolean)
    .map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"))
    .reduce((node, part) => node?.[part], root);
  if (target === undefined) throw new Error(`schema: unresolved $ref ${ref}`);
  return target;
}

function isType(type, value) {
  switch (type) {
    case "object":
      return value !== null && typeof value === "object" && !Array.isArray(value);
    case "array":
      return Array.isArray(value);
    case "string":
      return typeof value === "string";
    case "boolean":
      return typeof value === "boolean";
    case "null":
      return value === null;
    case "number":
      return Number.isFinite(value);
    case "integer":
      return Number.isInteger(value);
    default:
      return false;
  }
}

function equal(a, b) {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

function article(type) {
  return /^[aeiou]/.test(type) ? `an ${type}` : `a ${type}`;
}

function join(path, key) {
  return path ? `${path}.${key}` : key;
}

function fail(context, path, expected, value) {
  context.errors.push(`${path || "(root)"}: expected ${expected}, got ${JSON.stringify(value)}`);
}
