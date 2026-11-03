import assert from "node:assert/strict";
import test from "node:test";
import { guardBatch, guardChunk } from "./protected_span_guard.mjs";

const row = (overrides) => ({
  input_text: "Please send invoice SX-7109 to Amina tomorrow morning.",
  output_text: "Please send invoice SX-7109 to Amina tomorrow morning.",
  structured_spans: { protected: [{ kind: "identifier", input_surface: "SX-7109", target_surface: "SX-7109" }, { kind: "name", input_surface: "Amina", target_surface: "Amina" }] },
  bounded_context: {},
  ...overrides,
});

test("clean output passes through untouched", () => {
  const result = guardChunk(row({ output_text: "Please send invoice SX-7109 to Amina tomorrow morning." }));
  assert.equal(result.status, "pass");
  assert.equal(result.text, "Please send invoice SX-7109 to Amina tomorrow morning.");
});

test("a dropped protected span falls back to the original chunk", () => {
  const result = guardChunk(row({ output_text: "Please send the invoice to Amina tomorrow morning." }));
  assert.equal(result.status, "fallback");
  assert.equal(result.text, "Please send invoice SX-7109 to Amina tomorrow morning.");
  assert.match(result.reason, /unrepairable protected span: SX-7109/u);
});

test("a length stop always falls back, even when the text looks fine", () => {
  const result = guardChunk(row({ finish_reason: "length" }));
  assert.equal(result.status, "fallback");
  assert.match(result.reason, /token cap/u);
});

test("an un-normalized span is repaired deterministically and re-verified", () => {
  const result = guardChunk({
    input_text: "email team 75 at birch dot test today",
    output_text: "Email team 75 at birch dot test today.",
    structured_spans: { protected: [{ kind: "email", input_surface: "team 75 at birch dot test", target_surface: "team75@birch.test" }] },
    bounded_context: {},
  });
  assert.equal(result.status, "repaired");
  assert.deepEqual(result.repairedSpans, ["team75@birch.test"]);
  assert.equal(result.text, "Email team75@birch.test today.");
});

test("a repair that would leave the output violating a product rule falls back", () => {
  const result = guardChunk(row({ output_text: "Output: Please send invoice SX-7109 to Amina tomorrow morning." }));
  assert.equal(result.status, "fallback");
  assert.match(result.reason, /wrapper/u);
});

test("empty output falls back", () => {
  assert.equal(guardChunk(row({ output_text: "   " })).status, "fallback");
});

test("batch reports the fallback rate the 2% budget is measured against", () => {
  const report = guardBatch([
    row({ id: "a" }),
    row({ id: "b" }),
    row({ id: "c", output_text: "Please send the invoice to Amina." }),
    row({ id: "d" }),
  ]);
  assert.equal(report.total, 4);
  assert.equal(report.counts.pass, 3);
  assert.equal(report.counts.fallback, 1);
  assert.equal(report.fallbackRate, 0.25);
});
