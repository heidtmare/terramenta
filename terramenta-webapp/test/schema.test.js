import assert from "node:assert/strict";
import { test } from "node:test";
import { validate } from "../src/schema.js";

const errors = (schema, value, warnings = []) => validate(schema, value, { warn: (path) => warnings.push(path) });

test("type, const and enum", () => {
  assert.deepEqual(errors({ type: "integer" }, 1), []);
  assert.deepEqual(errors({ type: "integer" }, 1.5), ["(root): expected an integer, got 1.5"]);
  assert.deepEqual(errors({ type: ["array", "null"] }, null), []);
  assert.deepEqual(errors({ type: "number" }, Number.NaN), ["(root): expected a number, got null"]);
  assert.deepEqual(errors({ const: 2 }, 1), ["(root): expected 2, got 1"]);
  assert.deepEqual(errors({ enum: ["a", "b"] }, "c"), ['(root): expected one of a, b, got "c"']);
});

test("a failed type check skips the other keywords", () => {
  assert.deepEqual(errors({ type: "number", minimum: 0, enum: [1] }, "x"), ['(root): expected a number, got "x"']);
});

test("numeric bounds", () => {
  assert.deepEqual(errors({ minimum: 0 }, 0), []);
  assert.deepEqual(errors({ exclusiveMinimum: 0 }, 0), ["(root): expected a number > 0, got 0"]);
  assert.deepEqual(errors({ maximum: 90 }, 91), ["(root): expected a number ≤ 90, got 91"]);
  assert.deepEqual(errors({ exclusiveMaximum: 1 }, 1), ["(root): expected a number < 1, got 1"]);
});

test("pattern and date-time; title names the expectation", () => {
  assert.deepEqual(errors({ pattern: "^#[0-9a-f]{6}$", title: "a hex colour" }, "red"), [
    '(root): expected a hex colour, got "red"',
  ]);
  assert.deepEqual(errors({ format: "date-time" }, "2026-10-13T00:00:00Z"), []);
  assert.deepEqual(errors({ format: "date-time" }, "soon"), ['(root): expected an ISO 8601 date-time, got "soon"']);
});

test("objects: required, properties, unknown keys warn", () => {
  const schema = {
    type: "object",
    required: ["id"],
    additionalProperties: false,
    properties: { id: { type: "string" }, inner: { type: "object", properties: { n: { type: "number" } } } },
  };
  const warnings = [];
  assert.deepEqual(errors(schema, { inner: { n: "1" }, extra: 1 }, warnings), [
    "id: required",
    'inner.n: expected a number, got "1"',
  ]);
  assert.deepEqual(warnings, ["extra"]);
  assert.deepEqual(errors({ additionalProperties: { type: "boolean" } }, { a: 1 }), ["a: expected a boolean, got 1"]);
});

test("array items carry their index", () => {
  assert.deepEqual(errors({ items: { type: "integer" } }, [1, "2"]), ['[1]: expected an integer, got "2"']);
  assert.deepEqual(errors({ properties: { a: { items: { type: "integer" } } } }, { a: [0, 0.5] }), [
    "a[1]: expected an integer, got 0.5",
  ]);
});

test("$ref, anyOf, allOf and if/then", () => {
  const schema = {
    $defs: { when: { anyOf: [{ const: "now" }, { type: "string", format: "date-time" }] } },
    properties: {
      epoch: { $ref: "#/$defs/when" },
      both: { allOf: [{ type: "number" }, { minimum: 1 }] },
      tagged: {
        if: { properties: { kind: { const: "a" } } },
        then: { required: ["a"] },
        else: { required: ["b"] },
      },
    },
  };
  assert.deepEqual(errors(schema, { epoch: "now", both: 2, tagged: { kind: "a", a: 1 } }), []);
  assert.deepEqual(errors(schema, { epoch: 3, both: 0, tagged: { kind: "z" } }), [
    'epoch: expected "now" or an ISO 8601 date-time, got 3',
    "both: expected a number ≥ 1, got 0",
    "tagged.b: required",
  ]);
});

test("anyOf branches do not leak warnings", () => {
  const warnings = [];
  const schema = { anyOf: [{ type: "object", additionalProperties: false }, { type: "string" }] };
  assert.deepEqual(errors(schema, { a: 1 }, warnings), []);
  assert.deepEqual(warnings, []);
});

test("unresolvable $ref throws", () => {
  assert.throws(() => validate({ $ref: "#/$defs/missing" }, 1), /unresolved/);
  assert.throws(() => validate({ $ref: "other.json" }, 1), /only local/);
});

test("array length", () => {
  assert.deepEqual(errors({ minItems: 3, maxItems: 3 }, [1, 2, 3]), []);
  assert.deepEqual(errors({ minItems: 3, maxItems: 3 }, [1, 2]), ["(root): expected 3 items, got [1,2]"]);
  assert.deepEqual(errors({ minItems: 1 }, []), ["(root): expected ≥ 1 items, got []"]);
  assert.deepEqual(errors({ minItems: 1, maxItems: 2, items: { type: "string" } }, [1, 2, 3]), [
    "(root): expected 1–2 items, got [1,2,3]",
  ]);
});
