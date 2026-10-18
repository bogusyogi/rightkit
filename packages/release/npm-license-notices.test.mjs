import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { collectNpmPackages, escapeHtml, generateNpmNotices, parseNoticeArgs, renderNpmNotices, resolveNoticeOptions, SPDX_FALLBACK_TEXT } from "./npm-license-notices.mjs";

const groups = {
  MIT: [
    { name: "zeta", versions: ["1.0.0"], paths: ["/p/zeta"] },
    { name: "@rightkit/theme", versions: ["0.2.4"], paths: ["/p/theme"] },
    { name: "alpha", versions: ["2.0.0", "2.1.0"], paths: ["/p/alpha"] },
  ],
  ISC: [{ name: "isarray", versions: ["1.0.0"], paths: ["/p/isarray"] }],
  GPL: [{ name: "odd", versions: ["1.0.0"], paths: ["/p/odd"] }],
};

test("collectNpmPackages sorts by name and version and drops excluded prefixes", () => {
  const packages = collectNpmPackages({ MIT: groups.MIT, ISC: groups.ISC });
  assert.deepEqual(packages.map((p) => p.name), ["alpha", "isarray", "zeta"]);
  assert.equal(packages[0].license, "MIT");
  assert.equal(collectNpmPackages({ MIT: groups.MIT }, []).length, 3);
});

test("renderNpmNotices uses packaged license files, labelled fallbacks, and escapes HTML", () => {
  const files = { "/p/alpha": ["LICENSE", "README.md", "NOTICE.txt"], "/p/zeta": [], "/p/isarray": [] };
  const texts = { "/p/alpha/LICENSE": "Copyright <alpha> & co", "/p/alpha/NOTICE.txt": "notice text" };
  const packages = collectNpmPackages({ MIT: groups.MIT, ISC: groups.ISC });
  const out = renderNpmNotices({
    productName: "Demo <App>", version: "1.2.3", lockHash: "abc", packages,
    readNames: (dir) => files[dir.replaceAll("\\", "/")], readText: (file) => texts[file.replaceAll("\\", "/")],
  });
  assert.equal(out.packageCount, 3);
  assert.equal(out.fallbackCount, 2);
  assert.match(out.html, /<title>Demo &lt;App&gt; npm third-party licenses<\/title>/);
  assert.match(out.html, /Demo &lt;App&gt; 1\.2\.3; inputs: pnpm-lock\.yaml sha256=abc; generated-by=right-release license-notices npm/);
  assert.match(out.html, /<h2>alpha 2\.0\.0, 2\.1\.0<\/h2><p>Declared license: MIT<\/p><h3>LICENSE<\/h3><pre>Copyright &lt;alpha&gt; &amp; co<\/pre>/);
  assert.match(out.html, /<h3>NOTICE\.txt<\/h3><pre>notice text<\/pre>/);
  assert.ok(!out.html.includes("README.md"));
  assert.ok(out.html.includes(escapeHtml(SPDX_FALLBACK_TEXT.ISC)));
  assert.match(out.html, /No packaged license file found/);
  assert.ok(out.html.indexOf("<h2>alpha") < out.html.indexOf("<h2>isarray") && out.html.indexOf("<h2>isarray") < out.html.indexOf("<h2>zeta"));
});

test("a license with no file and no fallback text is an error, never invented text", () => {
  const packages = collectNpmPackages({ GPL: groups.GPL });
  assert.throws(() => renderNpmNotices({ productName: "A", version: "1", lockHash: "x", packages, readNames: () => [] }), /no SPDX fallback text for declared license "GPL"/);
});

test("resolveNoticeOptions merges config and flags and refuses bad input", () => {
  const config = { packageManager: "pnpm", legal: { npmNotices: { output: "legal/N.html", productName: "App" } } };
  assert.deepEqual(resolveNoticeOptions(config), { output: "legal/N.html", productName: "App", exclude: ["@rightkit/"] });
  assert.equal(resolveNoticeOptions(config, { output: "x/y.html", productName: "Other" }).output, "x/y.html");
  assert.throws(() => resolveNoticeOptions({}), /no output file/);
  assert.throws(() => resolveNoticeOptions({ legal: { npmNotices: { output: "a.html" } } }), /no product name/);
  assert.throws(() => resolveNoticeOptions({ legal: { npmNotices: { output: "../a.html", productName: "A" } } }), /stay inside the app/);
  assert.throws(() => resolveNoticeOptions({ legal: { npmNotices: { output: "a.html", productName: "A", extra: 1 } } }), /unknown key/);
  assert.throws(() => resolveNoticeOptions({ packageManager: "npm", legal: { npmNotices: { output: "a.html", productName: "A" } } }), /pnpm apps only/);
  assert.deepEqual(parseNoticeArgs(["npm", "--check", "--product-name", "P", "--output", "o.html"]), { config: "right-release.config.mjs", output: "o.html", productName: "P", check: true });
  assert.throws(() => parseNoticeArgs(["go"]), /expected/);
  assert.throws(() => parseNoticeArgs(["npm", "--nope"]), /unknown license-notices argument/);
});

test("generateNpmNotices writes the page from pnpm output, records the lock hash, and --check detects drift", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-npm-notices-"));
  try {
    const pkgDir = path.join(dir, "node_modules", "alpha");
    mkdirSync(pkgDir, { recursive: true });
    writeFileSync(path.join(pkgDir, "LICENSE"), "alpha license");
    writeFileSync(path.join(dir, "package.json"), JSON.stringify({ version: "4.5.6" }));
    writeFileSync(path.join(dir, "pnpm-lock.yaml"), "lock: 1\n");
    const config = { packageManager: "pnpm", legal: { npmNotices: { output: "legal/N.html", productName: "App" } } };
    const calls = [];
    const spawnSyncFn = (cmd, args) => {
      calls.push([cmd, ...args]);
      return { status: 0, stdout: JSON.stringify({ MIT: [{ name: "alpha", versions: ["1.0.0"], paths: [pkgDir] }, { name: "@rightkit/x", versions: ["1"], paths: [pkgDir] }] }) };
    };
    const written = generateNpmNotices({ appRoot: dir, config, spawnSyncFn });
    assert.deepEqual(calls[0], ["pnpm", "licenses", "list", "--prod", "--json"]);
    assert.equal(written.packageCount, 1);
    const html = readFileSync(path.join(dir, "legal", "N.html"), "utf8");
    const lockHash = createHash("sha256").update("lock: 1\n").digest("hex");
    assert.ok(html.includes(`App 4.5.6; inputs: pnpm-lock.yaml sha256=${lockHash}`));
    assert.ok(html.includes("alpha license"));
    assert.ok(!html.includes("@rightkit/x"));
    assert.equal(generateNpmNotices({ appRoot: dir, config, spawnSyncFn, check: true }).upToDate, true);
    writeFileSync(path.join(dir, "pnpm-lock.yaml"), "lock: 2\n");
    assert.equal(generateNpmNotices({ appRoot: dir, config, spawnSyncFn, check: true }).upToDate, false);
    assert.throws(() => generateNpmNotices({ appRoot: dir, config, spawnSyncFn: () => ({ status: 1, stderr: "boom" }) }), /boom/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
