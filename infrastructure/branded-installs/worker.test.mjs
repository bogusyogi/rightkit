import assert from "node:assert/strict";
import test from "node:test";
import worker, { objectKeyFor } from "./worker.mjs";

test("hostnames resolve only their product bootstrap", () => {
  assert.equal(objectKeyFor(new Request("https://legion.orthiclabs.com/install.ps1")), "legion/install.ps1");
  assert.equal(objectKeyFor(new Request("https://membrane.orthiclabs.com/install.ps1")), "membrane/install.ps1");
  assert.equal(objectKeyFor(new Request("https://legion.orthiclabs.com/membrane/install.ps1")), null);
  assert.equal(objectKeyFor(new Request("https://unknown.orthiclabs.com/install.ps1")), null);
});

test("worker serves only GET or HEAD with PowerShell content type", async () => {
  const env = { INSTALL_BUCKET: { async get(key) { return key === "legion/install.ps1" ? { body: "ok\n", httpEtag: '"sha"' } : null; } } };
  const get = await worker.fetch(new Request("https://legion.orthiclabs.com/install.ps1"), env);
  assert.equal(get.status, 200);
  assert.equal(get.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.equal(await get.text(), "ok\n");
  const post = await worker.fetch(new Request("https://legion.orthiclabs.com/install.ps1", { method: "POST" }), env);
  assert.equal(post.status, 405);
});

