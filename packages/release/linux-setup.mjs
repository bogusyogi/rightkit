// Idempotent Linux host provisioning for apps gated by `rightkit-gate` (rightkit#1365): reads the app's pins, installs Node
// (official tarball into ~/.local), pnpm (corepack, else npm into ~/.local), Rust (rustup), then fills the pnpm store for every
// committed lockroot with `pnpm fetch` against a throwaway virtual store, so an existing node_modules is never cleared.
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { parse as parseToml } from "smol-toml";

const EXACT = /^\d+\.\d+\.\d+$/;

export function readPins(root, read = (p) => (existsSync(p) ? readFileSync(p, "utf8") : null)) {
  const pkgText = read(join(root, "package.json"));
  const pkg = pkgText ? JSON.parse(pkgText) : {};
  const nodeFile = read(join(root, ".node-version"))?.trim().replace(/^v/, "");
  const node = nodeFile || String(pkg.engines?.node ?? "").trim() || null;
  const pm = /^pnpm@(\d+\.\d+\.\d+)/.exec(pkg.packageManager ?? "");
  const tomlText = read(join(root, "rust-toolchain.toml"));
  const rust = tomlText ? (parseToml(tomlText).toolchain?.channel ?? null) : null;
  return { node, pnpm: pm ? pm[1] : null, rust };
}

// "26.8.x" / "26.8" / "26" / "^26.8.0" / "26.8.1" -> newest matching release in the nodejs.org index (array of {version}).
export function resolveNodeVersion(spec, index) {
  if (!spec) return null;
  const clean = spec.replace(/^[v^~=]+/, "").replace(/\.x\b/g, "").replace(/\.\*/g, "");
  if (EXACT.test(clean)) return clean;
  const m = /^(\d+)(?:\.(\d+))?/.exec(clean);
  if (!m) throw new Error(`cannot resolve Node pin ${JSON.stringify(spec)}`);
  const found = index.map((e) => e.version.replace(/^v/, "")).filter((v) => {
    const [a, b] = v.split(".");
    return a === m[1] && (m[2] === undefined || b === m[2]);
  }).sort((x, y) => x.split(".").map(Number).reduce((d, n, i) => d || n - Number(y.split(".")[i]), 0) * -1);
  if (!found.length) throw new Error(`no Node release matches ${JSON.stringify(spec)}`);
  return found[0];
}

export function nodeArch(arch = process.arch) {
  const a = { x64: "x64", arm64: "arm64", arm: "armv7l" }[arch];
  if (!a) throw new Error(`unsupported CPU architecture ${arch}`);
  return a;
}

export function findLockroots(root, readDir = (d) => readdirSync(d, { withFileTypes: true })) {
  const out = [];
  const walk = (dir) => {
    const entries = readDir(dir);
    if (entries.some((e) => e.isFile() && e.name === "pnpm-lock.yaml")) out.push(relative(root, dir) || ".");
    for (const e of entries) {
      if (e.isDirectory() && !["node_modules", ".git", "target"].includes(e.name) && !e.name.startsWith(".")) walk(join(dir, e.name));
    }
  };
  walk(root);
  return out.sort();
}

// run(cmd, args, opts) -> {status, stdout}; fetchText(url) -> string. Both injectable so the plan is testable offline.
export function setup({ root, home = homedir(), arch = process.arch, run, fetchText, log = () => {}, env = process.env, dryRun = false } = {}) {
  root = resolve(root);
  const pins = readPins(root);
  const bin = join(home, ".local", "bin");
  const steps = [];
  const sh = (cmd, args, opts = {}) => {
    steps.push([cmd, ...args].join(" "));
    if (dryRun) return { status: 0, stdout: "" };
    const r = run(cmd, args, { cwd: root, ...opts, env: { ...env, ...opts.env, PATH: `${bin}:${join(home, ".cargo", "bin")}:${opts.env?.PATH ?? env.PATH ?? ""}` } });
    if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed (${r.status})`);
    return r;
  };
  const probe = (cmd, args) => { try { return (run(cmd, args, { cwd: root, env: { ...env, PATH: `${bin}:${join(home, ".cargo", "bin")}:${env.PATH ?? ""}` } }).stdout ?? "").trim(); } catch { return ""; } };

  if (pins.node) {
    const exact = EXACT.test(pins.node.replace(/^v/, ""));
    const version = dryRun && !exact ? null : resolveNodeVersion(pins.node, exact ? [] : JSON.parse(fetchText("https://nodejs.org/dist/index.json")));
    if (dryRun && !version) steps.push(`(install Node ${pins.node})`);
    if (version && probe("node", ["--version"]) !== `v${version}`) {
      const name = `node-v${version}-linux-${nodeArch(arch)}`;
      const dest = join(home, ".local");
      log(`Node ${version}`);
      sh("sh", ["-c", `set -e; mkdir -p "${dest}/lib" "${bin}"; cd "$(mktemp -d)"; curl -fsSLO https://nodejs.org/dist/v${version}/${name}.tar.xz; curl -fsSL https://nodejs.org/dist/v${version}/SHASUMS256.txt | grep " ${name}.tar.xz$" | sha256sum -c -; rm -rf "${dest}/lib/${name}"; tar -xJf ${name}.tar.xz -C "${dest}/lib"; for b in node npm npx corepack; do [ -e "${dest}/lib/${name}/bin/$b" ] && ln -sfn "${dest}/lib/${name}/bin/$b" "${bin}/$b"; done`]);
    }
  }
  if (pins.pnpm && probe("pnpm", ["--version"]) !== pins.pnpm) {
    log(`pnpm ${pins.pnpm}`);
    if (dryRun || probe("corepack", ["--version"])) {
      sh("corepack", ["enable", "--install-directory", bin]);
      sh("corepack", ["prepare", `pnpm@${pins.pnpm}`, "--activate"]);
    } else sh("npm", ["install", "-g", `pnpm@${pins.pnpm}`, "--prefix", join(home, ".local")]);
  }
  if (pins.rust) {
    if (!probe("rustup", ["--version"])) {
      log("rustup");
      sh("sh", ["-c", "curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path"]);
    }
    log(`Rust ${pins.rust}`);
    sh("rustup", ["toolchain", "install", "--profile", "minimal"]); // reads rust-toolchain.toml from the app root
  }
  const lockroots = findLockroots(root);
  for (const lr of lockroots) {
    const dir = resolve(root, lr);
    log(`pnpm store: ${lr}`);
    // The throwaway virtual store keeps `pnpm fetch` from touching the root's node_modules.
    const vstore = dryRun ? join(tmpdir(), "rk-vstore") : mkdtempSync(join(tmpdir(), "rk-vstore-"));
    try { sh("pnpm", ["fetch", "--virtual-store-dir", vstore], { cwd: dir }); } finally { if (!dryRun) rmSync(vstore, { recursive: true, force: true }); }
  }
  return { pins, lockroots, steps };
}
