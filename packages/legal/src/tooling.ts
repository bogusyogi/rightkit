import { createHash, randomUUID } from "node:crypto";
import { lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { assertLegalManifest, materialAcceptanceDigest } from "./index.js";
import type { LegalAppendixDescriptor, LegalDocumentDescriptor, LegalManifest } from "./core.js";

export type SnapshotDocument = Omit<LegalDocumentDescriptor, "sha256"> & { source?: string };
export type SnapshotAppendix = Omit<LegalAppendixDescriptor, "sha256">;
export interface LegalSnapshotConfig {
  appKey: string;
  productName: string;
  licensor: string;
  effectiveDate: string;
  acceptanceVersion: string;
  draftAcceptanceVersion: string;
  profile?: LegalManifest["profile"];
  /** Relative to appRoot, or absolute; required only when copying source documents. */
  sourceRoot?: string;
  documents: SnapshotDocument[];
  appendices?: SnapshotAppendix[];
  legalDir?: string;
  contractPath?: string;
  /** Document path inside legalDir, rendered from template. */
  notices?: { template: string; path: string; hashes: Record<string, string> };
}

const DRAFT = /DRAFT:\s*pending|\[TBD/i;
const hash = (bytes: string | Buffer) => createHash("sha256").update(bytes).digest("hex");
const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`;
const canonical = (value: unknown) => JSON.stringify(value, (_key, item: unknown) =>
  item && typeof item === "object" && !Array.isArray(item)
    ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0))
    : item,
);

function inside(root: string, relative: string): string {
  if (!relative || relative.includes("\0") || relative.includes("\\") || path.isAbsolute(relative) || /^[a-z]:/i.test(relative) || relative.split("/").includes("..")) {
    throw new Error(`expected safe relative path: ${relative}`);
  }
  return path.join(root, relative);
}

/**
 * Real location of `target`: every existing ancestor is resolved through symlinks (lstat, then realpath);
 * the missing tail is appended lexically. A dangling link is refused, never followed.
 */
function realTarget(target: string): string {
  const missing: string[] = [];
  let current = path.resolve(target);
  for (;;) {
    let exists = true;
    try {
      lstatSync(current);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      exists = false;
    }
    if (exists) {
      try {
        return path.join(realpathSync(current), ...missing.reverse());
      } catch {
        throw new Error(`refusing unresolvable link: ${current}`);
      }
    }
    const parent = path.dirname(current);
    if (parent === current) throw new Error(`no existing ancestor: ${target}`);
    missing.push(path.basename(current));
    current = parent;
  }
}

/** Refuse when the resolved target lies outside the resolved declared root (any symlinked ancestor included). */
function assertContained(root: string, target: string, label: string): void {
  const base = realTarget(root);
  const resolved = realTarget(target);
  const prefix = base.endsWith(path.sep) ? base : `${base}${path.sep}`;
  if (resolved !== base && !resolved.startsWith(prefix)) {
    throw new Error(`${label} resolves outside ${root}: ${target}`);
  }
}

/** Lexical safety check, then real-path containment within `root`. Returns the lexical path. */
function contained(root: string, relative: string, label = "path"): string {
  const file = inside(root, relative);
  assertContained(root, file, label);
  return file;
}

/** Writes only after the real target is confirmed inside `root`; the check precedes mkdir so no directory is created outside. */
function atomicWrite(root: string, file: string, bytes: string | Buffer): void {
  assertContained(root, file, "output");
  mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, bytes);
    renameSync(temporary, file);
  } finally {
    rmSync(temporary, { force: true });
  }
}

function snapshotPaths(config: LegalSnapshotConfig, appRoot: string) {
  const root = path.resolve(appRoot);
  return {
    root,
    legal: contained(root, config.legalDir ?? "legal", "legal directory"),
    contract: contained(root, config.contractPath ?? "src/generated/legal-contract.json", "contract path"),
  };
}

/** Copy masters, render notices, then write validated manifest & gate contract. No subprocesses. */
export function syncLegalSnapshot(config: LegalSnapshotConfig, appRoot = process.cwd()) {
  const { root, legal, contract } = snapshotPaths(config, appRoot);
  if (!config.draftAcceptanceVersion?.trim()) throw new Error("draftAcceptanceVersion is required");
  if (!config.acceptanceVersion?.trim()) throw new Error("acceptanceVersion is required");
  const files = new Map<string, Buffer>();
  const paths = [...config.documents, ...(config.appendices ?? [])].map((item) => item.path);
  if (new Set(paths).size !== paths.length) throw new Error("duplicate snapshot file path");
  for (const document of config.documents) {
    contained(legal, document.path, "document path");
    if (document.source && !config.sourceRoot) throw new Error("sourceRoot is required for source documents");
    if (document.path === config.notices?.path && !document.source) continue;
    // Source masters are contained in their declared sourceRoot, which may legitimately sit outside appRoot.
    files.set(document.path, readFileSync(document.source
      ? contained(path.resolve(root, config.sourceRoot!), document.source, "source document")
      : contained(legal, document.path, "document")));
  }
  const appendices = (config.appendices ?? []).map((appendix) => {
    const bytes = readFileSync(contained(legal, appendix.path, "appendix"));
    return { ...appendix, sha256: hash(bytes) };
  });
  if (config.notices) {
    const { template, path: output, hashes } = config.notices;
    if (!config.documents.some((document) => document.path === output && !document.source)) {
      throw new Error("notices.path must name a document without source");
    }
    const fill: Record<string, string> = {
      effectiveDate: config.effectiveDate,
      version: JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version,
    };
    for (const [key, file] of Object.entries(hashes)) {
      if (key in fill) throw new Error(`reserved notices placeholder: ${key}`);
      // Hash inputs may live outside appRoot (e.g. engine Cargo.lock).
      fill[key] = hash(readFileSync(path.resolve(root, file)));
    }
    const text = readFileSync(contained(legal, template, "notices template"), "utf8").replace(/\{\{(\w+)\}\}/g, (_, key: string) => {
      if (!Object.hasOwn(fill, key)) throw new Error(`unknown notices placeholder {{${key}}}`);
      return fill[key]!;
    });
    files.set(output, Buffer.from(text));
  }
  const draft = config.documents.some((document) => DRAFT.test(files.get(document.path)!.toString("utf8")));
  const manifest = assertLegalManifest({
    schema: 2, suite: "right-suite-desktop", appKey: config.appKey,
    productName: config.productName, licensor: config.licensor, effectiveDate: config.effectiveDate,
    acceptanceVersion: draft ? config.draftAcceptanceVersion : config.acceptanceVersion,
    ...(config.profile ? { profile: config.profile } : {}),
    documents: config.documents.map(({ source: _source, ...document }) => ({
      ...document, sha256: hash(files.get(document.path)!),
    })),
    appendices,
  });
  const gate = { acceptanceVersion: manifest.acceptanceVersion, materialSha256: materialAcceptanceDigest(manifest), draft };
  // Resolve & validate every input before changing snapshot files.
  for (const [relative, bytes] of files) atomicWrite(legal, inside(legal, relative), bytes);
  atomicWrite(legal, path.join(legal, "legal-manifest.json"), json(manifest));
  atomicWrite(root, contract, json(gate));
  return { manifest, contract: gate };
}

/** Read-only release check: drafts, missing/stale hashes & stale gate contracts fail. No masters needed. */
export function assertFinalLegalSnapshot(config: LegalSnapshotConfig, appRoot = process.cwd()): LegalManifest {
  const { legal, contract } = snapshotPaths(config, appRoot);
  const manifest = assertLegalManifest(JSON.parse(readFileSync(contained(legal, "legal-manifest.json", "manifest"), "utf8")));
  const expected = config.documents.map(({ source: _source, ...document }) => document);
  const actual = manifest.documents.map(({ sha256: _sha256, ...document }) => document);
  if (canonical(expected) !== canonical(actual) || canonical(config.appendices ?? []) !== canonical((manifest.appendices ?? []).map(({ sha256: _sha256, ...appendix }) => appendix)) ||
      manifest.productName !== config.productName || manifest.licensor !== config.licensor || manifest.effectiveDate !== config.effectiveDate ||
      (manifest.profile ?? "proprietary") !== (config.profile ?? "proprietary")) {
    throw new Error("legal: snapshot descriptors differ from config; sync first");
  }
  for (const item of [...manifest.documents, ...(manifest.appendices ?? [])]) {
    const bytes = readFileSync(contained(legal, item.path, "snapshot document"));
    if (DRAFT.test(bytes.toString("utf8"))) throw new Error(`legal: not releasable, still draft/[TBD]: ${item.path}`);
    if (hash(bytes) !== item.sha256.toLowerCase()) throw new Error(`legal: hash mismatch: ${item.path}; sync first`);
  }
  if (manifest.appKey !== config.appKey || manifest.acceptanceVersion !== config.acceptanceVersion) {
    throw new Error("legal: snapshot appKey/acceptanceVersion differs from final config; sync first");
  }
  const expectedGate = { acceptanceVersion: manifest.acceptanceVersion, materialSha256: materialAcceptanceDigest(manifest), draft: false };
  const gate = JSON.parse(readFileSync(contract, "utf8"));
  if (Object.entries(expectedGate).some(([key, value]) => gate[key] !== value)) throw new Error("legal: stale gate contract; sync first");
  return manifest;
}

export interface NpmLicenseEntry { name: string; versions: string[]; paths: string[] }
export type NpmLicenseGroups = Record<string, NpmLicenseEntry[]>;
export interface NpmLicenseAppendixOptions {
  appRoot?: string;
  productName?: string;
  output?: string;
  /** Supply captured `pnpm licenses list --prod --json` output, skipping subprocess execution. */
  groups?: NpmLicenseGroups;
}

// Standard SPDX boilerplate; used only when packaged license files are absent.
const SPDX_FALLBACK_TEXT: Readonly<Record<string, string>> = Object.freeze({
  MIT: `Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.`,
  ISC: `Permission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted, provided that the above copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.`,
});

const escapeHtml = (value: string) => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");

/** Generate production npm notices, excluding @rightkit/*, with escaped texts & lockfile provenance. */
export function generateNpmLicenseAppendix(options: NpmLicenseAppendixOptions = {}) {
  const root = path.resolve(options.appRoot ?? process.cwd());
  const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  const productName = options.productName ?? pkg.productName ?? pkg.name;
  if (typeof productName !== "string" || !productName.trim()) throw new Error("productName is required");
  let groups = options.groups;
  if (!groups) {
    const result = spawnSync("pnpm", ["licenses", "list", "--prod", "--json"], {
      cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, shell: process.platform === "win32",
    });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(result.stderr || "pnpm licenses list failed");
    groups = JSON.parse(result.stdout) as NpmLicenseGroups;
  }
  const packages = Object.entries(groups).flatMap(([license, entries]) => entries.map((entry) => ({ ...entry, license })))
    .filter((entry) => !entry.name.startsWith("@rightkit/"))
    .sort((a, b) => {
      const left = `${a.name}@${a.versions.join(",")}`;
      const right = `${b.name}@${b.versions.join(",")}`;
      return left < right ? -1 : left > right ? 1 : 0;
    });
  let fallbackCount = 0;
  const sections = packages.map((entry) => {
    if (!entry.paths?.length) throw new Error(`${entry.name}: missing installed package path`);
    // Read every distinct installed instance: versions can carry different notices.
    const texts = [...new Set(entry.paths)].sort().map((packagePath) => {
      const packageRoot = path.resolve(root, packagePath);
      assertContained(root, packageRoot, "installed package path");
      const files = readdirSync(packageRoot, { withFileTypes: true })
        .filter((file) => file.isFile() && /^(?:licen[cs]e|copying|notice)(?:[._-]|$)/i.test(file.name))
        .map((file) => file.name).sort();
      if (files.length) return files.map((file) => `<h3>${escapeHtml(file)}</h3><pre>${escapeHtml(readFileSync(path.join(packageRoot, file), "utf8"))}</pre>`).join("\n");
      const fallback = SPDX_FALLBACK_TEXT[entry.license];
      if (!fallback) throw new Error(`${entry.name} ${entry.versions.join(", ")} has no packaged license/notice file and no SPDX fallback for "${entry.license}"`);
      fallbackCount++;
      return `<h3>No packaged license file found</h3><p><em>This package's installed npm distribution does not include a LICENSE/COPYING/NOTICE file. The text below is the standard published license text for its declared SPDX license (${escapeHtml(entry.license)}), not package-specific content.</em></p><pre>${escapeHtml(fallback)}</pre>`;
    }).join("\n");
    return `<section><h2>${escapeHtml(entry.name)} ${escapeHtml(entry.versions.join(", "))}</h2><p>Declared license: ${escapeHtml(entry.license)}</p>${texts}</section>`;
  });
  const lockHash = hash(readFileSync(path.join(root, "pnpm-lock.yaml")));
  const title = escapeHtml(`${productName} npm third-party licenses`);
  const html = `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>${title}</title></head><body>\n<h1>${title}</h1>\n<p>Build provenance: ${escapeHtml(productName)} ${escapeHtml(String(pkg.version))}; inputs: pnpm-lock.yaml sha256=${lockHash}; generated-by=@rightkit/legal/npm-license-appendix</p>\n${sections.join("\n")}\n</body></html>\n`;
  const output = inside(root, options.output ?? "legal/THIRD_PARTY_NPM_LICENSES.html");
  atomicWrite(root, output, html);
  return { output, packageCount: packages.length, fallbackCount };
}
