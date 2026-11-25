import { createHash, randomUUID } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
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

/** Where an app's canonical masters live, by manifest document role. The app supplies every file name. */
export interface ManifestSnapshotConfig {
  /** Directory holding the canonical masters (the app resolves it: an environment override, a sibling checkout). */
  mastersDir: string;
  /** Role -> file name directly under `mastersDir`, shared by every app. */
  shared: Readonly<Record<string, string>>;
  /** Role -> file name under `<mastersDir>/<productsDir>/<manifest.appKey>`, specific to the product. */
  product: Readonly<Record<string, string>>;
  /** Default `products`. */
  productsDir?: string;
  /** Default `legal`. */
  legalDir?: string;
}

export interface ManifestSnapshotPlan {
  manifest: LegalManifest;
  manifestPath: string;
  legalDir: string;
  /** Snapshot documents whose bytes differ from their master (or are absent). */
  copies: { src: string; dest: string; bytes: Buffer; id: string }[];
  /** Every document and appendix with the sha256 its bytes will have after the copies. */
  hashes: { entry: { id: string; sha256: string }; sha: string }[];
  /** Ids whose manifest sha256 differs from `hashes`. */
  manifestDrift: string[];
}

/**
 * Plan a manifest-driven snapshot sync, writing nothing. The committed manifest is the source of truth for
 * descriptors, versions, titles and `acceptanceVersion`; a document whose role names a master is compared with it,
 * every other document and appendix is only re-hashed. A missing master is an error.
 */
export function planManifestSnapshot(config: ManifestSnapshotConfig, appRoot = process.cwd()): ManifestSnapshotPlan {
  const legalDir = path.join(path.resolve(appRoot), config.legalDir ?? "legal");
  const manifestPath = path.join(legalDir, "legal-manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as LegalManifest;
  const copies: ManifestSnapshotPlan["copies"] = [];
  const hashes: ManifestSnapshotPlan["hashes"] = [];
  for (const doc of manifest.documents) {
    const dest = path.join(legalDir, doc.path);
    const shared = config.shared[doc.role];
    const name = shared ?? config.product[doc.role];
    if (name) {
      const src = shared
        ? path.join(config.mastersDir, name)
        : path.join(config.mastersDir, config.productsDir ?? "products", manifest.appKey, name);
      if (!existsSync(src)) throw new Error(`canonical master missing: ${src}`);
      const bytes = readFileSync(src);
      const current = existsSync(dest) ? readFileSync(dest) : null;
      if (!current?.equals(bytes)) copies.push({ src, dest, bytes, id: doc.id });
      hashes.push({ entry: doc, sha: hash(bytes) });
    } else {
      hashes.push({ entry: doc, sha: hash(readFileSync(dest)) });
    }
  }
  for (const appendix of manifest.appendices ?? []) {
    hashes.push({ entry: appendix, sha: hash(readFileSync(path.join(legalDir, appendix.path))) });
  }
  const manifestDrift = hashes.filter(({ entry, sha }) => entry.sha256 !== sha).map(({ entry }) => entry.id);
  return { manifest, manifestPath, legalDir, copies, hashes, manifestDrift };
}

/** Write a plan: copy the masters, rebind every sha256 in the manifest (2-space JSON, trailing newline). */
export function applyManifestSnapshot(plan: ManifestSnapshotPlan): void {
  for (const copy of plan.copies) atomicWrite(plan.legalDir, copy.dest, copy.bytes);
  for (const { entry, sha } of plan.hashes) entry.sha256 = sha;
  atomicWrite(plan.legalDir, plan.manifestPath, json(plan.manifest));
}

/** One label per pending change, `copy <id>` or `rehash <id>`; empty when the snapshot matches its masters and the manifest. */
export function manifestSnapshotChanges(plan: ManifestSnapshotPlan): string[] {
  return [...plan.copies.map((copy) => `copy ${copy.id}`), ...plan.manifestDrift.map((id) => `rehash ${id}`)];
}

export interface LegalGateContractConfig {
  /** Default `legal`. */
  legalDir?: string;
  /** Default `src/generated/legal-contract.json`. */
  contractPath?: string;
  /** The agreement checkbox wording: copy, supplied by the app. */
  agreementStatement: string;
  /** Document roles the gate lists, in manifest order. The manifest must hold exactly one document per role. */
  roles: readonly string[];
}

export interface LegalGateContract {
  acceptanceVersion: string;
  materialSha256: string;
  agreementStatement: string;
  documents: { title: string; url: string }[];
}

/**
 * Write the in-app acceptance gate contract from the committed manifest: `acceptanceVersion`, the material digest,
 * the agreement statement and one `{ title, url }` per listed role. The manifest is validated first and never edited.
 * (`syncLegalSnapshot` writes a narrower `{ acceptanceVersion, materialSha256, draft }` contract to the same default path.)
 */
export function syncLegalGateContract(config: LegalGateContractConfig, appRoot = process.cwd()): LegalGateContract {
  const root = path.resolve(appRoot);
  const legal = contained(root, config.legalDir ?? "legal", "legal directory");
  const target = contained(root, config.contractPath ?? "src/generated/legal-contract.json", "contract path");
  const manifest = assertLegalManifest(JSON.parse(readFileSync(contained(legal, "legal-manifest.json", "manifest"), "utf8")));
  if (typeof manifest.acceptanceVersion !== "string" || !manifest.acceptanceVersion.trim()) {
    throw new Error("legal manifest acceptanceVersion is missing");
  }
  const roles = new Set<string>(config.roles);
  const documents = manifest.documents
    .filter((document) => roles.has(document.role))
    .map((document) => ({ title: document.title, url: document.publicUrl }));
  if (documents.length !== roles.size) {
    throw new Error(`legal manifest must contain exactly the ${roles.size} checkout-role documents`);
  }
  const contract: LegalGateContract = {
    acceptanceVersion: manifest.acceptanceVersion,
    materialSha256: materialAcceptanceDigest(manifest),
    agreementStatement: config.agreementStatement,
    documents,
  };
  atomicWrite(root, target, json(contract));
  return contract;
}

/** What a notice provenance rewrite or requirement may quote: the app version and the hashes it binds. */
export interface NoticeProvenanceContext {
  /** `version` of the app's package.json. */
  version: string;
  /** sha256 of each lock input, keyed by the path as configured in `locks`. */
  hashes: Record<string, string>;
  /** sha256 of each appendix file, keyed by appendix id. */
  appendices: Record<string, string>;
}

/** A line of the notices document that must match `pattern` exactly once and is replaced by `text`. */
export interface NoticeProvenanceRewrite {
  label: string;
  pattern: RegExp;
  text: (context: NoticeProvenanceContext) => string;
}

/** Text a snapshot file (relative to `legalDir`) must contain. */
export interface NoticeProvenanceRequirement {
  file: string;
  label: string;
  text: (context: NoticeProvenanceContext) => string;
}

export interface NoticeProvenanceConfig {
  /** Default `legal`. */
  legalDir?: string;
  /** Default `package.json`, relative to the app root. */
  packageJson?: string;
  /** Lock files whose sha256 is bound, relative to the app root (or absolute). */
  locks: readonly string[];
  /** The notices document inside `legalDir`; `role` defaults to `third_party_notices`; `label` names it in drift errors. */
  notices: { path: string; role?: string; label: string };
  /** Generated appendices inside `legalDir`; `label` names each in drift errors. */
  appendices: readonly { id: string; path: string; label: string }[];
  /** Lines `syncNoticeProvenance` rewrites, in order. */
  rewrites: readonly NoticeProvenanceRewrite[];
  /** Text `assertNoticeProvenance` requires, checked in order. */
  requirements: readonly NoticeProvenanceRequirement[];
}

function noticeProvenance(config: NoticeProvenanceConfig, root: string, legal: string): NoticeProvenanceContext {
  const version: unknown = JSON.parse(readFileSync(contained(root, config.packageJson ?? "package.json", "package.json"), "utf8")).version;
  if (typeof version !== "string") throw new Error("package.json version is missing");
  return {
    version,
    hashes: Object.fromEntries(config.locks.map((lock) => [lock, hash(readFileSync(path.resolve(root, lock)))])),
    appendices: Object.fromEntries(config.appendices.map((appendix) => [appendix.id, hash(readFileSync(contained(legal, appendix.path, "appendix")))])),
  };
}

function replaceOnce(source: string, pattern: RegExp, replacement: string, label: string): string {
  const global = new RegExp(pattern.source, pattern.flags.includes("g") ? pattern.flags : `${pattern.flags}g`);
  const matches = source.match(global);
  if (matches?.length !== 1) throw new Error(`expected exactly one ${label}, found ${matches?.length ?? 0}`);
  return source.replace(pattern, () => replacement);
}

/**
 * Rebind a committed notices document to the app version and the lock and appendix hashes: every rewrite must match
 * exactly one line. Then the notices hash and each appendix hash are written into the manifest, which is checked before
 * anything is written. Notices come first, manifest second, both atomically.
 */
export function syncNoticeProvenance(config: NoticeProvenanceConfig, appRoot = process.cwd()) {
  const root = path.resolve(appRoot);
  const legal = contained(root, config.legalDir ?? "legal", "legal directory");
  const context = noticeProvenance(config, root, legal);
  const noticesFile = contained(legal, config.notices.path, "notices document");
  let notices = readFileSync(noticesFile, "utf8");
  for (const rewrite of config.rewrites) notices = replaceOnce(notices, rewrite.pattern, rewrite.text(context), rewrite.label);
  const manifestFile = contained(legal, "legal-manifest.json", "manifest");
  const manifest = JSON.parse(readFileSync(manifestFile, "utf8")) as LegalManifest;
  const noticeDocument = manifest.documents.find((entry) => entry.role === (config.notices.role ?? "third_party_notices"));
  const appendices = config.appendices.map((appendix) => manifest.appendices?.find((entry) => entry.id === appendix.id));
  if (!noticeDocument || appendices.some((entry) => !entry)) throw new Error("legal manifest is missing third-party notice entries");
  const noticeHash = hash(notices);
  noticeDocument.sha256 = noticeHash;
  config.appendices.forEach((appendix, index) => { appendices[index]!.sha256 = context.appendices[appendix.id]!; });
  atomicWrite(legal, noticesFile, notices);
  atomicWrite(legal, manifestFile, json(manifest));
  return { ...context, noticeHash };
}

/**
 * Read-only check that the notices document, the generated appendices and the manifest carry the current version
 * and hashes: each requirement text must appear in its file, then the manifest hashes must equal the files'.
 * Renders nothing and writes nothing; throws on the first drift.
 */
export function assertNoticeProvenance(config: NoticeProvenanceConfig, appRoot = process.cwd()) {
  const root = path.resolve(appRoot);
  const legal = contained(root, config.legalDir ?? "legal", "legal directory");
  const context = noticeProvenance(config, root, legal);
  const files = new Map<string, string>();
  const text = (file: string) => {
    if (!files.has(file)) files.set(file, readFileSync(contained(legal, file, "snapshot file"), "utf8"));
    return files.get(file)!;
  };
  // Every file is read before any text is compared, so a missing file is reported ahead of drift.
  for (const file of new Set([config.notices.path, ...config.requirements.map((requirement) => requirement.file)])) text(file);
  const manifest = JSON.parse(readFileSync(contained(legal, "legal-manifest.json", "manifest"), "utf8")) as LegalManifest;
  for (const requirement of config.requirements) {
    const expected = requirement.text(context);
    if (!text(requirement.file).includes(expected)) throw new Error(`${requirement.label} drift: expected ${expected}`);
  }
  const noticeHash = hash(readFileSync(contained(legal, config.notices.path, "notices document")));
  const noticeDocument = manifest.documents.find((entry) => entry.role === (config.notices.role ?? "third_party_notices"));
  if (noticeDocument?.sha256 !== noticeHash) throw new Error(`legal-manifest ${config.notices.label} hash drift`);
  for (const appendix of config.appendices) {
    if (manifest.appendices?.find((entry) => entry.id === appendix.id)?.sha256 !== context.appendices[appendix.id]) {
      throw new Error(`legal-manifest ${appendix.label} hash drift`);
    }
  }
  return { ...context, noticeHash };
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

interface PnpmListNode { version?: string; path?: string; dependencies?: Record<string, PnpmListNode>; optionalDependencies?: Record<string, PnpmListNode> }

/**
 * Every package reachable from a project's dependencies or optionalDependencies in `pnpm list --json --depth Infinity` output,
 * following resolved peer edges too: pnpm 12's `licenses list --prod` stopped following a peer edge (an optional peer of a production
 * package that the app also installs as a dev dependency), which dropped `@rightkit/release`'s own dependencies from the notices.
 */
export function npmProductionClosure(tree: unknown): Map<string, { name: string; version: string; path?: string }> {
  if (!Array.isArray(tree)) throw new Error("pnpm list --json did not return a project array");
  const closure = new Map<string, { name: string; version: string; path?: string }>(), seen = new Set<string>();
  const visit = (name: string, node?: PnpmListNode) => {
    if (!node || typeof node !== "object" || typeof node.version !== "string" || node.version.startsWith("link:")) return;
    const id = `${name}@${node.version}`, at = `${id}|${node.path ?? ""}`;
    if (seen.has(at)) return;
    seen.add(at);
    if (!closure.has(id)) closure.set(id, { name, version: node.version, path: node.path });
    for (const section of ["dependencies", "optionalDependencies"] as const) for (const [child, value] of Object.entries(node[section] ?? {})) visit(child, value);
  };
  for (const project of tree as PnpmListNode[]) for (const section of ["dependencies", "optionalDependencies"] as const) for (const [name, node] of Object.entries(project?.[section] ?? {})) visit(name, node);
  return closure;
}

/** `groups` plus every closure package it lacks, licensed from the full listing; a package the full listing lacks is an error, never a shorter notice. */
export function completeNpmProductionGroups(groups: NpmLicenseGroups, closure: ReturnType<typeof npmProductionClosure>, loadAll: () => NpmLicenseGroups): NpmLicenseGroups {
  const listed = new Set(Object.values(groups).flatMap((entries) => entries.flatMap((entry) => entry.versions.map((v) => `${entry.name}@${v}`))));
  const missing = [...closure].filter(([id, item]) => !listed.has(id) && !item.name.startsWith("@rightkit/"));
  if (!missing.length) return groups;
  const all = loadAll(), completed: NpmLicenseGroups = Object.fromEntries(Object.entries(groups).map(([license, entries]) => [license, [...entries]]));
  for (const [id, item] of missing) {
    const hit = Object.entries(all).find(([, entries]) => entries.some((entry) => entry.name === item.name && entry.versions.includes(item.version)));
    if (!hit || typeof item.path !== "string") throw new Error(`${id} is in the production closure (pnpm list) but not in pnpm licenses list: its notice cannot be produced`);
    (completed[hit[0]] ??= []).push({ name: item.name, versions: [item.version], paths: [item.path] });
  }
  return completed;
}

/** Generate production npm notices, excluding @rightkit/*, with escaped texts & lockfile provenance. */
export function generateNpmLicenseAppendix(options: NpmLicenseAppendixOptions = {}) {
  const root = path.resolve(options.appRoot ?? process.cwd());
  const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  const productName = options.productName ?? pkg.productName ?? pkg.name;
  if (typeof productName !== "string" || !productName.trim()) throw new Error("productName is required");
  let groups = options.groups;
  if (!groups) {
    const run = (args: string[]): unknown => {
      const result = spawnSync("pnpm", args, {
        cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, shell: process.platform === "win32",
      });
      if (result.error) throw result.error;
      if (result.status !== 0) throw new Error(result.stderr || `pnpm ${args.join(" ")} failed`);
      return JSON.parse(result.stdout);
    };
    groups = completeNpmProductionGroups(
      run(["licenses", "list", "--prod", "--json"]) as NpmLicenseGroups,
      npmProductionClosure(run(["list", "--json", "--depth", "Infinity"])),
      () => run(["licenses", "list", "--json"]) as NpmLicenseGroups,
    );
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
