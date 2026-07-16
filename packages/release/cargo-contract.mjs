import { existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const SKIP_DIRECTORIES = new Set([".audit", ".cache", ".git", ".claude", ".worktrees", "node_modules", "target", "vendor"]);
const CRATES_IO_SOURCES = new Set([
  "registry+https://github.com/rust-lang/crates.io-index",
  "registry+https://index.crates.io/",
]);

export function validateRightKitCargoContract(root, publishedVersions, label = path.basename(root)) {
  const scanRoot = path.resolve(root);
  const boundary = findRepositoryRoot(scanRoot);
  const published = publishedVersions instanceof Map
    ? publishedVersions
    : new Map(Object.entries(publishedVersions ?? {}));
  const manifests = findCargoManifests(scanRoot);
  const cargoHome = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-home-"));
  try {
    for (const manifestPath of manifests) {
      const manifestLabel = path.relative(boundary, manifestPath) || "Cargo.toml";
      assertNoRightKitCargoOverrides(manifestPath, boundary, `${label}/${manifestLabel}`);
      const dependencies = readCargoManifestDependencies(manifestPath, cargoHome, `${label}/${manifestLabel}`);
      assertPublishedRightKitCargoDependencies(dependencies, published, `${label}/${manifestLabel}`);
    }
  } finally {
    rmSync(cargoHome, { recursive: true, force: true });
  }
  return manifests.length;
}

export function assertPublishedRightKitCargoDependencies(dependencies, published, label) {
  let checked = 0;
  for (const dependency of dependencies.filter(({ name }) => name.startsWith("rightkit-"))) {
    checked += 1;
    const configured = published.get(dependency.name);
    if (!configured) throw new Error(`${label} ${dependency.name} is not a published RightKit crate`);
    const allowed = configured instanceof Set ? configured : new Set(Array.isArray(configured) ? configured : [configured]);
    const exactVersions = new Set([...allowed].map((version) => `=${version}`));
    if (!CRATES_IO_SOURCES.has(dependency.source) || dependency.path || dependency.registry || !exactVersions.has(dependency.req)) {
      throw new Error(`${label} ${dependency.name} must use exact crates.io versions (${[...exactVersions].join(" or ")})`);
    }
  }
  return checked;
}

export function assertNoRightKitCargoOverrides(manifestPath, repoRoot, label) {
  const boundary = path.resolve(repoRoot);
  const manifest = path.resolve(manifestPath);
  assertInsideBoundary(manifest, boundary);
  assertNoRightKitOverrides(readToml(manifest, label), label, path.relative(boundary, manifest) || "Cargo.toml");

  for (const configPath of findRepoCargoConfigs(path.dirname(manifest), boundary)) {
    const source = path.relative(boundary, configPath);
    const parsed = readToml(configPath, source);
    assertNoRightKitOverrides(parsed, label, source);
    const cratesIo = parsed.source?.["crates-io"];
    if (cratesIo && typeof cratesIo === "object"
      && ["replace-with", "directory", "git", "registry", "local-registry"].some((key) => key in cratesIo)) {
      throw new Error(`${label} has forbidden crates.io source replacement in ${source}`);
    }
  }
}

function readCargoManifestDependencies(manifestPath, cargoHome, label) {
  const result = spawnSync(
    "cargo",
    ["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path", manifestPath],
    {
      cwd: path.dirname(manifestPath),
      encoding: "utf8",
      env: { ...process.env, CARGO_HOME: cargoHome },
      windowsHide: true,
    },
  );
  if (result.status !== 0) {
    throw new Error(`${label} Cargo metadata rejected manifest; RightKit dependencies must use exact crates.io versions: ${String(result.stderr ?? "").trim()}`);
  }
  const metadata = JSON.parse(result.stdout);
  const expectedPath = path.resolve(manifestPath).toLowerCase();
  const pkg = metadata.packages.find(({ manifest_path: candidate }) => path.resolve(candidate).toLowerCase() === expectedPath);
  if (!pkg && path.resolve(metadata.workspace_root, "Cargo.toml").toLowerCase() === expectedPath) return [];
  if (!pkg) throw new Error(`${label} was not returned by Cargo metadata`);
  return pkg.dependencies;
}

function assertNoRightKitOverrides(parsed, label, source) {
  for (const overrides of Object.values(parsed.patch ?? {})) {
    if (!overrides || typeof overrides !== "object" || Array.isArray(overrides)) continue;
    for (const [name, specifier] of Object.entries(overrides)) {
      const packageName = typeof specifier === "object" && specifier ? specifier.package : undefined;
      if (name.startsWith("rightkit-") || packageName?.startsWith("rightkit-")) {
        throw new Error(`${label} has forbidden RightKit Cargo override ${name} in ${source}`);
      }
    }
  }
  for (const [name, specifier] of Object.entries(parsed.replace ?? {})) {
    const packageName = typeof specifier === "object" && specifier ? specifier.package : undefined;
    if (name.split(":", 1)[0].startsWith("rightkit-") || packageName?.startsWith("rightkit-")) {
      throw new Error(`${label} has forbidden RightKit Cargo override ${name} in ${source}`);
    }
  }
}

// `tomllib` is Python 3.11+. Windows pins `py -3.11`, but a bare `python3` on macOS is the system
// 3.9, which has no tomllib — so every app's release:doctor died on a stock Mac, and reported the
// manifest as invalid TOML when the real problem was the interpreter. Probe for one that can.
let tomlRunner;
function resolveTomlRunner() {
  if (tomlRunner) return tomlRunner;
  const probe = "import sys,tomllib";
  const candidates =
    process.platform === "win32"
      ? [["py", ["-3.11"]], ["py", ["-3"]], ["python", []]]
      : [["python3.13", []], ["python3.12", []], ["python3.11", []], ["python3", []]];
  for (const [command, prefix] of candidates) {
    const check = spawnSync(command, [...prefix, "-c", probe], { encoding: "utf8", windowsHide: true });
    if (check.status === 0) {
      tomlRunner = { command, prefix };
      return tomlRunner;
    }
  }
  throw new Error(
    "no Python with tomllib found (needs 3.11+). Install one, or put it on PATH: the release " +
      "contract reads Cargo manifests through it.",
  );
}

function readToml(file, label) {
  const script = "import json,pathlib,sys,tomllib; json.dump(tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding='utf-8')),sys.stdout,default=str)";
  const { command, prefix } = resolveTomlRunner();
  const result = spawnSync(command, [...prefix, "-c", script, file], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`${label} is not valid TOML: ${String(result.stderr ?? "").trim()}`);
  return JSON.parse(result.stdout);
}

function findCargoManifests(root) {
  const found = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && SKIP_DIRECTORIES.has(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile() && entry.name === "Cargo.toml") found.push(full);
    }
  };
  visit(root);
  return found;
}

function findRepoCargoConfigs(start, repoRoot) {
  const boundary = path.resolve(repoRoot);
  let current = path.resolve(start);
  assertInsideBoundary(current, boundary);
  const configs = [];
  while (true) {
    for (const filename of ["config", "config.toml"]) {
      const candidate = path.join(current, ".cargo", filename);
      if (existsSync(candidate)) configs.push(candidate);
    }
    if (current.toLowerCase() === boundary.toLowerCase()) break;
    current = path.dirname(current);
  }
  return configs;
}

function assertInsideBoundary(candidate, boundary) {
  const relative = path.relative(boundary, candidate);
  if (relative !== "" && (relative.startsWith("..") || path.isAbsolute(relative))) {
    throw new Error(`${candidate} is outside ${boundary}`);
  }
}

function findRepositoryRoot(start) {
  const original = path.resolve(start);
  let current = original;
  while (true) {
    if (existsSync(path.join(current, ".git"))) return current;
    const parent = path.dirname(current);
    if (parent === current) return original;
    current = parent;
  }
}
