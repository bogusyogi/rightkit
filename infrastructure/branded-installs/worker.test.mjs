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


test("plain HTTP is redirected to HTTPS", async () => {
  const res = await worker.fetch(new Request("http://legion.orthiclabs.com/install.ps1"), {});
  assert.equal(res.status, 301);
  assert.equal(res.headers.get("location"), "https://legion.orthiclabs.com/install.ps1");
});

test("every HTTPS response carries HSTS", async () => {
  const env = { INSTALL_BUCKET: { async get(key) { return key === "legion/install.ps1" ? { body: "ok\n", httpEtag: '"sha"' } : null; } } };
  const expected = "max-age=63072000; includeSubDomains; preload";
  const ok = await worker.fetch(new Request("https://legion.orthiclabs.com/install.ps1"), env);
  assert.equal(ok.headers.get("strict-transport-security"), expected);
  const missing = await worker.fetch(new Request("https://membrane.orthiclabs.com/install.ps1"), env);
  assert.equal(missing.status, 404);
  assert.equal(missing.headers.get("strict-transport-security"), expected);
  const denied = await worker.fetch(new Request("https://legion.orthiclabs.com/install.ps1", { method: "POST" }), env);
  assert.equal(denied.status, 405);
  assert.equal(denied.headers.get("allow"), "GET, HEAD");
  assert.equal(denied.headers.get("strict-transport-security"), expected);
  const root = await worker.fetch(new Request("https://legion.orthiclabs.com/"), env);
  assert.equal(root.status, 302);
  assert.equal(root.headers.get("location"), "https://legion.orthiclabs.com/install.ps1");
  assert.equal(root.headers.get("strict-transport-security"), expected);
});
