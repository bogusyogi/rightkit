// Resolve only manifest-explicit Cargo membership from authenticated commit bytes.
// Unsupported implicit/path-dependency membership fails closed.
import path from "node:path";
import { createRequire } from "node:module";
const parse = (text) => createRequire(import.meta.url)("smol-toml").parse(text);

export function committedCargoWorkspace({ unitDir, read }) {
  if (!/^crates\/[A-Za-z0-9_-]+$/.test(unitDir)) throw new Error("unsupported crate path");
  const get = (file) => {
    const value = read(file);
    if (value === null) return null;
    if (value === undefined) throw new Error(`unavailable committed manifest ${file}`);
    return parse(String(value));
  };
  const unit = get(`${unitDir}/Cargo.toml`);
  if (!unit?.package) throw new Error(`missing package manifest ${unitDir}`);
  if (get("crates/Cargo.toml") !== null) throw new Error("nested crates workspace requires explicit resolution");
  const explicit = unit.package.workspace;
  if (explicit !== undefined) {
    if (typeof explicit !== "string") throw new Error("invalid package.workspace");
    const resolved = path.posix.normalize(path.posix.join(unitDir, explicit));
    if (resolved !== "." && resolved !== unitDir) throw new Error("unsupported package.workspace location");
    const owner = resolved === "." ? get("Cargo.toml") : unit;
    if (!owner?.workspace) throw new Error("package.workspace is not a workspace");
    if (resolved === unitDir) return unitDir;
  }
  if (unit.workspace) return unitDir;
  const root = get("Cargo.toml");
  if (!root?.workspace) throw new Error("workspace membership cannot be established");
  const matches = (patterns) => {
    if (patterns === undefined) return false;
    if (!Array.isArray(patterns) || patterns.some((p) => typeof p !== "string")) throw new Error("invalid workspace members or exclude");
    return patterns.some((p) => {
      if (!/^[A-Za-z0-9_.*\/-]+$/.test(p) || p.includes("**") || p.split("/").includes("..")) throw new Error("unsupported workspace glob");
      const regex = new RegExp(`^${p.split("*").map((s) => s.replace(/[.]/g, "\\.")).join("[^/]*")}/?$`);
      return regex.test(unitDir);
    });
  };
  if (matches(root.workspace.exclude)) throw new Error("excluded crate has no explicit workspace");
  if (!matches(root.workspace.members)) throw new Error("implicit workspace membership cannot be established");
  return ".";
}

const sourceReads = new WeakMap();
// Candidate source reader authenticates blobs and signals absence with this precise error.
export function candidateWorkspace(source, unitDir) {
  if (typeof source?.readData !== "function") throw new Error("authenticated source reader missing");
  let cache = sourceReads.get(source);
  if (!cache) { cache = new Map(); sourceReads.set(source, cache); }
  return committedCargoWorkspace({ unitDir, read: (name) => {
    if (cache.has(name)) return cache.get(name);
    let value;
    try { value = source.readData(name).toString("utf8"); }
    catch (error) { if (error.message === `candidate missing ${name}`) value = null; else throw error; }
    cache.set(name, value);
    return value;
  } });
}
