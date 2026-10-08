import assert from "node:assert/strict";
import test from "node:test";
import { cargoIndexUrl, verifyCargoRegistry } from "./cargo-registry-readback.mjs";

const name = "rightkit-fixture", version = "1.0.0", sha256 = "a".repeat(64);
const options = { name, version, sha256 };
const index = (...entries) => ({ ok: true, status: 200, text: async () => entries.map((entry) => JSON.stringify(entry)).join("\n") + "\n" });
const matching = { name, vers: version, cksum: sha256 };
function clock() {
  let elapsed = 0;
  const waits = [];
  return { waits, now: () => elapsed, sleep: async (ms) => { waits.push(ms); elapsed += ms; } };
}

test("sparse index URL follows all standard prefix rules & lowercase names", () => {
  for (const [crate, location] of [["a", "1/a"], ["ab", "2/ab"], ["abc", "3/a/abc"], ["AbCd", "ab/cd/abcd"], [name, "ri/gh/rightkit-fixture"]]) {
    assert.equal(cargoIndexUrl(crate), `https://index.crates.io/${location}`);
  }
  assert.throws(() => cargoIndexUrl("../secret"), /invalid crate name/);
});

test("registry readback matches exact version checksum with unauthenticated GET", async () => {
  const result = await verifyCargoRegistry(options, {
    fetch: async (url, request) => {
      assert.equal(url, "https://index.crates.io/ri/gh/rightkit-fixture");
      assert.equal(request.method, undefined);
      assert.equal(request.body, undefined);
      assert.equal(request.headers.Authorization, undefined);
      assert.equal(request.headers["Cache-Control"], "no-cache");
      assert.ok(request.signal instanceof AbortSignal);
      return index({ ...matching, vers: "0.9.0", cksum: "b".repeat(64) }, matching);
    },
  });
  assert.deepEqual(result, options);
});

test("registry mismatch throws typed error with both hashes & operator yank instruction", async () => {
  const registrySha256 = "b".repeat(64);
  let requests = 0;
  await assert.rejects(verifyCargoRegistry(options, {
    fetch: async () => { requests++; return index({ ...matching, cksum: registrySha256 }); },
    sleep: () => { throw new Error("mismatch must not retry"); },
  }), (error) => {
    assert.equal(error.code, "PUBLICATION_REGISTRY_MISMATCH");
    assert.equal(error.expectedSha256, sha256);
    assert.equal(error.registrySha256, registrySha256);
    assert.ok(error.message.includes(sha256));
    assert.ok(error.message.includes(registrySha256));
    assert.match(error.message, /Operator must yank/);
    assert.match(error.message, /no automatic yank/);
    return true;
  });
  assert.equal(requests, 1);
});

test("missing registry checksum is a mismatch", async () => {
  await assert.rejects(verifyCargoRegistry(options, { fetch: async () => index({ name, vers: version }) }), { code: "PUBLICATION_REGISTRY_MISMATCH" });
});

test("index not yet visible then version appears after backoff", async () => {
  const timing = clock();
  let requests = 0;
  const result = await verifyCargoRegistry(options, { ...timing, fetch: async () => {
    requests++;
    if (requests === 1) return { ok: false, status: 404 };
    if (requests === 2) return index({ ...matching, vers: "0.9.0" }, { ...matching, name: "other-crate" });
    return index(matching);
  } });
  assert.deepEqual(result, options);
  assert.equal(requests, 3);
  assert.deepEqual(timing.waits, [1_000, 2_000]);
});

test("transient transport, HTTP & invalid index responses retry within deadline", async () => {
  const timing = clock();
  let requests = 0;
  assert.deepEqual(await verifyCargoRegistry(options, { ...timing, fetch: async () => {
    requests++;
    if (requests === 1) throw new Error("network unavailable");
    if (requests === 2) return { ok: false, status: 503 };
    if (requests === 3) return { ok: true, text: async () => "invalid JSON" };
    return index(matching);
  } }), options);
  assert.deepEqual(timing.waits, [1_000, 2_000, 4_000]);
});

test("registry visibility timeout is typed & default deadline is five minutes", async () => {
  const timing = clock();
  let requests = 0;
  await assert.rejects(verifyCargoRegistry(options, { ...timing, fetch: async () => {
    requests++;
    return index({ ...matching, vers: "0.9.0" });
  } }), (error) => error.code === "PUBLICATION_REGISTRY_TIMEOUT" && /within 300000ms/.test(error.message));
  assert.equal(timing.now(), 300_000);
  assert.deepEqual(timing.waits.slice(0, 5), [1_000, 2_000, 4_000, 8_000, 15_000]);
  assert.ok(timing.waits.every((ms) => ms <= 15_000));
  assert.equal(requests, timing.waits.length);
});

test("retry waits stop at remaining deadline", async () => {
  const timing = clock();
  await assert.rejects(verifyCargoRegistry(options, { ...timing, timeoutMs: 2_500, fetch: async () => ({ ok: false, status: 404 }) }),
    { code: "PUBLICATION_REGISTRY_TIMEOUT" });
  assert.deepEqual(timing.waits, [1_000, 1_500]);
});
