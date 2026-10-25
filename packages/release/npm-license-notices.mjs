#!/usr/bin/env node
// `right-release license-notices npm [--config <file>] [--output <file>] [--product-name <name>] [--check]`: writes the
// third-party npm license appendix (one HTML file) for an app's production dependency closure, from
// `pnpm licenses list --prod --json`, the installed packages' own LICENSE / COPYING / NOTICE files, and the lockfile hash.
//
//   legal: { npmNotices: { output: "legal/THIRD_PARTY_NPM_LICENSES.html", productName: "ViewRight", exclude: ["@rightkit/"] } }
//
// `output` and `productName` may also come from `--output` / `--product-name`. `exclude` is a list of package-name prefixes left
// out (default `["@rightkit/"]`). A package that ships no license file gets the standard published text for its declared SPDX
// license (MIT or ISC), labelled as a fallback; any other license without a file is an error, never invented text.
// The page records the app version, the sha256 of `pnpm-lock.yaml` and `generated-by=right-release license-notices npm`, so the
// same inputs always produce the same bytes. `--check` writes nothing and exits 1 when the file on disk differs.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";

const fail = (message) => { throw new CommandSpecError(message); };

// Generic published SPDX text, used ONLY when an installed package ships no license file of its own.
export const SPDX_FALLBACK_TEXT = {
  MIT: `Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.`,
  ISC: `Permission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted, provided that the above copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.`,
};

export const escapeHtml = (value) => String(value)
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;");

/** Flatten `pnpm licenses list --json` (license -> entries) into a sorted list, minus excluded name prefixes. */
export function collectNpmPackages(groups, exclude = ["@rightkit/"]) {
  return Object.entries(groups)
    .flatMap(([license, entries]) => entries.map((entry) => ({ ...entry, license })))
    .filter((entry) => !exclude.some((prefix) => entry.name.startsWith(prefix)))
    .sort((a, b) => `${a.name}@${a.versions.join(",")}`.localeCompare(`${b.name}@${b.versions.join(",")}`));
}

/**
 * Render the appendix. `readNames(dir)` lists a package directory and `readText(file)` reads one file (both injectable).
 * Returns `{ html, packageCount, fallbackCount }`.
 */
export function renderNpmNotices({ productName, version, lockHash, packages, readNames = (dir) => readdirSync(dir), readText = (file) => readFileSync(file, "utf8") }) {
  let fallbackCount = 0;
  const sections = packages.map((entry) => {
    const packageRoot = entry.paths[0];
    const files = readNames(packageRoot).filter((name) => /^(?:license|copying|notice)(?:[._-]|$)/i.test(name)).sort();
    let texts;
    if (files.length) {
      texts = files.map((name) => `<h3>${escapeHtml(name)}</h3><pre>${escapeHtml(readText(path.join(packageRoot, name)))}</pre>`).join("\n");
    } else {
      const fallback = SPDX_FALLBACK_TEXT[entry.license];
      if (!fallback) {
        fail(`${entry.name} ${entry.versions.join(", ")} has no packaged license/notice file and no SPDX fallback text for declared license "${entry.license}"; bundle the file or extend SPDX_FALLBACK_TEXT`);
      }
      fallbackCount += 1;
      texts = `<h3>No packaged license file found</h3><p><em>This package's installed npm distribution does not include a LICENSE/COPYING/NOTICE file. The text below is the standard published license text for its declared SPDX license (${escapeHtml(entry.license)}), not package-specific content.</em></p><pre>${escapeHtml(fallback)}</pre>`;
    }
    return `<section><h2>${escapeHtml(entry.name)} ${escapeHtml(entry.versions.join(", "))}</h2><p>Declared license: ${escapeHtml(entry.license)}</p>${texts}</section>`;
  });
  const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>${escapeHtml(productName)} npm third-party licenses</title></head><body>
<h1>${escapeHtml(productName)} npm third-party licenses</h1>
<p>Build provenance: ${escapeHtml(productName)} ${escapeHtml(version)}; inputs: pnpm-lock.yaml sha256=${lockHash}; generated-by=right-release license-notices npm</p>
${sections.join("\n")}
</body></html>\n`;
  return { html, packageCount: packages.length, fallbackCount };
}

/** Resolve options from config + flags. */
export function resolveNoticeOptions(config, flags = {}) {
  const declared = config?.legal?.npmNotices ?? {};
  if (typeof declared !== "object" || declared === null || Array.isArray(declared)) fail("legal.npmNotices must be an object");
  const unknown = Object.keys(declared).filter((key) => !["output", "productName", "exclude"].includes(key));
  if (unknown.length) fail(`legal.npmNotices has unknown key(s): ${unknown.join(", ")} (allowed: output, productName, exclude)`);
  const output = flags.output ?? declared.output;
  const productName = flags.productName ?? declared.productName;
  const exclude = declared.exclude ?? ["@rightkit/"];
  if (output === undefined) fail("no output file: set legal.npmNotices.output or pass --output");
  relativeInside(output, "legal.npmNotices.output");
  if (typeof productName !== "string" || !productName.trim()) fail("no product name: set legal.npmNotices.productName or pass --product-name");
  if (!Array.isArray(exclude) || exclude.some((prefix) => typeof prefix !== "string")) fail("legal.npmNotices.exclude must be a list of name prefixes");
  if ((config?.packageManager ?? "pnpm") !== "pnpm") fail(`license-notices npm supports pnpm apps only (config.packageManager is ${config.packageManager})`);
  return { output, productName, exclude };
}

/** Generate (or check) the appendix for the app at `appRoot`. `spawnSyncFn` is injectable. */
export function generateNpmNotices({ appRoot, config, flags = {}, check = false, spawnSyncFn = spawnSync }) {
  const { output, productName, exclude } = resolveNoticeOptions(config, flags);
  const version = JSON.parse(readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
  const result = spawnSyncFn("pnpm", ["licenses", "list", "--prod", "--json"], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    shell: process.platform === "win32",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) fail(result.stderr || "pnpm licenses list failed");
  const packages = collectNpmPackages(JSON.parse(result.stdout), exclude);
  const lockHash = createHash("sha256").update(readFileSync(path.join(appRoot, "pnpm-lock.yaml"))).digest("hex");
  const { html, packageCount, fallbackCount } = renderNpmNotices({ productName, version, lockHash, packages });
  const target = path.join(appRoot, output);
  if (check) {
    const current = existsSync(target) ? readFileSync(target, "utf8") : null;
    return { target, packageCount, fallbackCount, upToDate: current === html };
  }
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, html);
  return { target, packageCount, fallbackCount, upToDate: true };
}

export function parseNoticeArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), output: undefined, productName: undefined, check: false };
  const rest = argv[0] === "npm" ? argv.slice(1) : argv;
  if (argv[0] !== "npm") throw new Error("expected: license-notices npm");
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--config" || arg === "--output" || arg === "--product-name") {
      const value = rest[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg === "--product-name" ? "productName" : arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--check") opts.check = true;
    else throw new Error(`unknown license-notices argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseNoticeArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  const outcome = generateNpmNotices({ appRoot, config, flags: { output: opts.output, productName: opts.productName }, check: opts.check });
  if (opts.check) {
    if (!outcome.upToDate) {
      console.error(`right-release license-notices: ${outcome.target} is out of date; run right-release license-notices npm`);
      return 1;
    }
    console.log(`${outcome.target} is up to date (${outcome.packageCount} package notices)`);
    return 0;
  }
  console.log(`wrote ${outcome.packageCount} package notices (${outcome.fallbackCount} used an SPDX fallback text)`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release license-notices: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
