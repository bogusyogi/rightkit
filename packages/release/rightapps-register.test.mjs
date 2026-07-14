import assert from "node:assert/strict";
import test from "node:test";

import { registerRightAppsRelease } from "./rightapps-register.mjs";

test("registers through the shared durable release-token boundary", async () => {
  const calls = [];
  const response = await registerRightAppsRelease("/v1/admin/apps/runtime-artifacts", { schema: 1 }, {
    token: "durable-token",
    env: { RIGHTAPPS_API_URL: "https://rightapps.example", RIGHTAPPS_STORE_SLUG: "rightapps" },
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      return { ok: true, text: async () => '{"id":"registered"}' };
    },
  });
  assert.deepEqual(response, { id: "registered" });
  assert.equal(calls[0].url, "https://rightapps.example/v1/admin/apps/runtime-artifacts");
  assert.equal(calls[0].init.headers.authorization, "Bearer durable-token");
});

test("rejects absent durable token and non-admin registration paths before network", async () => {
  let called = false;
  const fetchImpl = async () => { called = true; };
  await assert.rejects(registerRightAppsRelease("/v1/admin/apps/runtime-artifacts", {}, { token: "", fetchImpl }), /release token/i);
  await assert.rejects(registerRightAppsRelease("https://evil.invalid", {}, { token: "token", fetchImpl }), /route/i);
  await assert.rejects(registerRightAppsRelease("/v1/admin/apps/../../evil", {}, { token: "token", fetchImpl }), /route/i);
  assert.equal(called, false);
});
