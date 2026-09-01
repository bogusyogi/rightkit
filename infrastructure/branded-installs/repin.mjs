#!/usr/bin/env node
/**
 * Repin a branded bootstrap to an exact published release.
 *
 * The stable R2 objects drifted to Legion 0.3.4 and Membrane 0.1.12 because
 * repinning was a manual edit of three literals. This derives all three from
 * the published release itself and refuses to write unless the downloaded
 * asset's SHA-256 matches what it is about to pin, so a bootstrap can never
 * name bytes that were not verified at pin time.
 *
 * Usage: node repin.mjs <product> <version> [--asset <name>]
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

export const PRODUCTS = Object.freeze({
  legion: { repo: "Orthic-Labs/legion", assetPattern: (v) => `Legion-${v}-windows-x86_64-setup.exe` },
  membrane: { repo: "Orthic-Labs/Membrane", assetPattern: (v) => `Membrane.Hub_${v}_x64-setup.exe` },
});

const SEMVER = /^\d+\.\d+\.\d+$/;

function fail(message) { throw new Error(message); }

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/**
 * Rewrites the three pinned literals. Returns the new text; never writes.
 * Each replacement must match exactly once, so a bootstrap whose shape has
 * drifted fails loudly instead of being silently half-updated.
 */
export function repinText(text, { version, uri, digest }) {
  const edits = [
    [/^\$Version = '[^']*'$/m, `$Version = '${version}'`],
    [/^\$Uri = '[^']*'$/m, `$Uri = '${uri}'`],
    [/^\$ExpectedSha256 = '[^']*'$/m, `$ExpectedSha256 = '${digest}'`],
  ];
  let result = text;
  for (const [pattern, replacement] of edits) {
    const matches = result.match(new RegExp(pattern.source, "gm")) ?? [];
    if (matches.length !== 1) fail(`bootstrap must contain exactly one ${pattern.source} line, found ${matches.length}`);
    result = result.replace(pattern, replacement);
  }
  return result;
}

export function repin(product, version, { assetName, runner = spawnSync, write = true } = {}) {
  const spec = PRODUCTS[product];
  if (!spec) fail(`unknown product: ${product}; expected ${Object.keys(PRODUCTS).join(" or ")}`);
  if (!SEMVER.test(String(version ?? ""))) fail(`version must be exact semver, got: ${version}`);

  const bootstrap = resolve(HERE, "bootstraps", product, "install.ps1");
  if (!existsSync(bootstrap)) fail(`bootstrap is missing: ${bootstrap}`);

  const tag = `v${version}`;
  const asset = assetName ?? spec.assetPattern(version);
  const scratch = mkdtempSync(join(tmpdir(), `rightkit-repin-${product}-`));
  try {
    const download = runner("gh", ["release", "download", tag, "--repo", spec.repo, "--pattern", asset, "--dir", scratch, "--clobber"], { windowsHide: true, encoding: "utf8" });
    if (download?.error || download?.status !== 0) fail(`could not download ${asset} from ${spec.repo} ${tag}: ${download?.stderr ?? download?.error?.message ?? "gh failed"}`);
    const downloaded = join(scratch, asset);
    if (!existsSync(downloaded)) {
      const found = readdirSync(scratch).join(", ") || "nothing";
      fail(`expected asset ${asset} was not downloaded (got: ${found})`);
    }
    const digest = sha256(downloaded);
    const uri = `https://github.com/${spec.repo}/releases/download/${tag}/${asset}`;
    const next = repinText(readFileSync(bootstrap, "utf8"), { version, uri, digest });
    if (write) writeFileSync(bootstrap, next, "utf8");
    return { product, version, tag, asset, uri, digest, bootstrap, written: write };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const invokedDirectly = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  const [, , product, version] = process.argv;
  const assetIndex = process.argv.indexOf("--asset");
  if (!product || !version) {
    process.stderr.write("usage: node repin.mjs <legion|membrane> <version> [--asset <name>]\n");
    process.exit(1);
  }
  const result = repin(product, version, { assetName: assetIndex > 0 ? process.argv[assetIndex + 1] : undefined });
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}
