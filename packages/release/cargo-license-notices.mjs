#!/usr/bin/env node
// `right-release license-notices cargo [--config <file>] [--output <file>] [--check]`: writes the third-party Cargo license appendix
// (one file) for an app's Rust dependency closure with `cargo about`, run through the managed cargo client (`runCargoSync`: the
// RightKit broker on a managed host, never a bare-cargo fallback on a managed Mac).
//
//   legal: { cargoNotices: { output: "legal/THIRD_PARTY_CARGO_LICENSES.html", template: "legal/about.hbs",
//                            config?: "legal/about.toml", manifestPath?: "src-tauri/Cargo.toml", features?: ["a"], allFeatures?: true } }
//
// `output`, `template`, `config` and `manifestPath` are app-relative and may not leave the app. The run is `cargo about generate --frozen
// --fail [--config <config>] [--manifest-path <path>] [--all-features | --features a,b] <template>`; cargo-about writes the rendered
// appendix to stdout, which is normalised to LF and one trailing newline so the same lockfile always produces the same bytes. `--fail`
// makes an unaccepted license or an unresolved clarification an error instead of a warning. `--check` writes nothing and exits 1 when
// the file on disk differs.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { runCargoSync } from "./managed-cargo.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const KEYS = ["output", "template", "config", "manifestPath", "features", "allFeatures"];

/** Resolve and validate `legal.cargoNotices` plus flags. */
export function resolveCargoNoticeOptions(config, flags = {}) {
  const declared = config?.legal?.cargoNotices ?? {};
  if (typeof declared !== "object" || declared === null || Array.isArray(declared)) fail("legal.cargoNotices must be an object");
  const unknown = Object.keys(declared).filter((key) => !KEYS.includes(key));
  if (unknown.length) fail(`legal.cargoNotices has unknown key(s): ${unknown.join(", ")} (allowed: ${KEYS.join(", ")})`);
  const output = flags.output ?? declared.output;
  if (output === undefined) fail("no output file: set legal.cargoNotices.output or pass --output");
  relativeInside(output, "legal.cargoNotices.output");
  if (declared.template === undefined) fail("legal.cargoNotices.template (the cargo-about handlebars template) is required");
  relativeInside(declared.template, "legal.cargoNotices.template");
  for (const key of ["config", "manifestPath"]) if (declared[key] !== undefined) relativeInside(declared[key], `legal.cargoNotices.${key}`);
  const features = declared.features ?? [];
  if (!Array.isArray(features) || features.some((feature) => typeof feature !== "string" || !/^[A-Za-z0-9_/-]+$/.test(feature))) fail("legal.cargoNotices.features must be a list of feature names");
  if (declared.allFeatures !== undefined && typeof declared.allFeatures !== "boolean") fail("legal.cargoNotices.allFeatures must be a boolean");
  if (declared.allFeatures === true && features.length) fail("legal.cargoNotices: give allFeatures or features, not both");
  return { output, template: declared.template, config: declared.config, manifestPath: declared.manifestPath, features, allFeatures: declared.allFeatures === true };
}

/** The `cargo about` argument list (after the managed client adds `cargo`). */
export function cargoAboutArgs(options) {
  return [
    "about", "generate", "--frozen", "--fail",
    ...(options.config ? ["--config", options.config] : []),
    ...(options.manifestPath ? ["--manifest-path", options.manifestPath] : []),
    ...(options.allFeatures ? ["--all-features"] : options.features.length ? ["--features", options.features.join(",")] : []),
    options.template,
  ];
}

const normalise = (text) => `${String(text).replace(/\r\n/g, "\n").replace(/\s+$/, "")}\n`;

/** Generate (or check) the Cargo appendix for the app at `appRoot`. `run` is the managed cargo runner (injectable). */
export function generateCargoNotices({ appRoot, config, flags = {}, check = false, run = runCargoSync, env = process.env, exists = existsSync, readText = (file) => readFileSync(file, "utf8"), writeText = (file, text) => { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, text); } }) {
  const options = resolveCargoNoticeOptions(config, flags);
  for (const key of ["template", "config", "manifestPath"]) {
    if (options[key] !== undefined && !exists(path.join(appRoot, options[key]))) fail(`legal.cargoNotices.${key} ${options[key]} does not exist`);
  }
  const result = run(cargoAboutArgs(options), { cwd: appRoot, env, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (result?.error) fail(`cargo about could not start (${result.error.message}); install cargo-about on the build host`);
  if (result?.status !== 0) fail(`cargo about exited ${result?.status ?? "without status"}${result?.stderr ? `:\n${String(result.stderr).trim()}` : ""}`);
  const text = normalise(result.stdout ?? "");
  if (text.trim() === "") fail("cargo about produced no output");
  const target = path.join(appRoot, options.output);
  if (check) {
    const current = exists(target) ? normalise(readText(target)) : null;
    return { target, bytes: Buffer.byteLength(text), upToDate: current === text };
  }
  writeText(target, text);
  return { target, bytes: Buffer.byteLength(text), upToDate: true };
}

export function parseCargoNoticeArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), output: undefined, check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--output") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--check") opts.check = true;
    else throw new Error(`unknown license-notices argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseCargoNoticeArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  const outcome = generateCargoNotices({ appRoot, config, flags: { output: opts.output }, check: opts.check });
  if (opts.check) {
    if (!outcome.upToDate) {
      console.error(`right-release license-notices: ${outcome.target} is out of date; run right-release license-notices cargo`);
      return 1;
    }
    console.log(`${outcome.target} is up to date`);
    return 0;
  }
  console.log(`wrote ${outcome.target} (${outcome.bytes} bytes)`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release license-notices: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
