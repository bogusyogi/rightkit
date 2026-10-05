import { createHash, randomUUID } from 'node:crypto';
import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { access, mkdir, open, readFile, readdir, rename, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { delimiter, join } from 'node:path';
import { promisify } from 'node:util';

// Prebuilt OpenSSL cache (plan Step 4). A vendored-OpenSSL build (openssl-src, perl Configure, nmake) is the
// largest native cost on Windows and its in-tree object paths brush MAX_PATH. This module builds OpenSSL once
// per full recipe key under a SHORT root, publishes it immutably, and lets the broker point openssl-sys at it
// with target-prefixed variables so the vendored build is skipped.

export const OPENSSL_RECIPE_VERSION = 1;
export const OPENSSL_OPT_OUT_ENV = 'RIGHTKIT_NATIVE_OPENSSL_CACHE';
const execFile = promisify(nodeExecFile);
const REQUIRED_LIBS = Object.freeze(['lib/libssl.lib', 'lib/libcrypto.lib']);
const OVERRIDE_NAMES = Object.freeze(['OPENSSL_DIR', 'OPENSSL_LIB_DIR', 'OPENSSL_INCLUDE_DIR']);

export const targetEnvPrefix = (triple) => String(triple).toUpperCase().replace(/-/g, '_');

/** Only Windows MSVC has a recipe today; macOS follows by adding an entry (libs, probes) here. */
export function recipeFor(triple) {
  if (typeof triple === 'string' && /-windows-msvc$/.test(triple)) return Object.freeze({ id: 'windows-msvc', libs: REQUIRED_LIBS });
  return null;
}

export function parseCargoLockPackages(text) {
  const packages = [];
  for (const block of String(text).split(/\r?\n\[\[package\]\]\r?\n/).slice(1)) {
    const field = (name) => new RegExp(`^${name} = "([^"]*)"`, 'm').exec(block)?.[1] ?? null;
    packages.push({ name: field('name'), version: field('version'), checksum: field('checksum') });
  }
  return packages;
}

/** Decide from Cargo.lock whether the OpenSSL cache applies. */
export function detectOpenSsl({ lockText, targetTriple }) {
  if (!recipeFor(targetTriple)) return { needed: false, reason: 'unsupported-target' };
  const packages = parseCargoLockPackages(lockText ?? '');
  const sources = packages.filter((entry) => entry.name === 'openssl-src');
  if (sources.length === 0) return { needed: false, reason: 'no-openssl-src' };
  if (sources.length > 1) return { needed: false, reason: 'multiple-openssl-src-versions' };
  const pick = (name) => packages.find((entry) => entry.name === name) ?? null;
  return { needed: true, opensslSrc: sources[0], opensslSys: pick('openssl-sys'), cc: pick('cc') };
}

export function findOverrides(environment, triple) {
  const prefix = targetEnvPrefix(triple);
  const found = [];
  for (const key of Object.keys(environment ?? {})) {
    const upper = key.toUpperCase();
    for (const name of OVERRIDE_NAMES) if (upper === name || upper === `${prefix}_${name}`) found.push(key);
  }
  return found;
}

export function injectionEnvironment(triple, directory) {
  const prefix = targetEnvPrefix(triple);
  return Object.freeze({ [`${prefix}_OPENSSL_DIR`]: directory, [`${prefix}_OPENSSL_NO_VENDOR`]: '1', [`${prefix}_OPENSSL_STATIC`]: '1' });
}

export function deriveKey(inputs) {
  const canonical = JSON.stringify({
    recipe: OPENSSL_RECIPE_VERSION,
    opensslSrc: { version: inputs.opensslSrc.version, checksum: inputs.opensslSrc.checksum },
    features: [...(inputs.features ?? [])].sort(),
    target: inputs.targetTriple,
    msvc: inputs.toolchain?.msvc ?? null,
    sdk: inputs.toolchain?.sdk ?? null,
    perl: inputs.toolchain?.perl ?? null,
    nasm: inputs.toolchain?.nasm ?? null,
    cc: inputs.cc ?? null
  });
  const key = createHash('sha256').update(canonical).digest('hex');
  return Object.freeze({ key, key16: key.slice(0, 16), canonical });
}

const sha256File = async (path) => createHash('sha256').update(await readFile(path)).digest('hex');

async function listFiles(root, base = root) {
  const files = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) files.push(...await listFiles(path, base));
    else if (entry.isFile()) files.push(path.slice(base.length + 1).replace(/\\/g, '/'));
  }
  return files.sort();
}

const exists = (path) => access(path).then(() => true, () => false);

async function findOnPath(names, pathValue) {
  for (const directory of String(pathValue ?? '').split(delimiter)) {
    if (!directory) continue;
    for (const name of names) { const candidate = join(directory, name); if (await exists(candidate)) return candidate; }
  }
  return null;
}

/** Toolchain facts that change OpenSSL's bytes: MSVC toolset (cl.exe banner), SDK, perl, nasm. */
export async function probeWindowsToolchain({ environment = process.env, exec = execFile, programFilesX86 = process.env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)' } = {}) {
  const run = async (file, args, options = {}) => { try { const { stdout, stderr } = await exec(file, args, { encoding: 'utf8', timeout: 30_000, windowsHide: true, ...options }); return `${stdout ?? ''}${stderr ?? ''}`; } catch (error) { return `${error?.stdout ?? ''}${error?.stderr ?? ''}` || null; } };
  const result = { msvc: null, sdk: null, perl: null, nasm: null };
  const vswhere = join(programFilesX86, 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
  if (await exists(vswhere)) {
    const install = (await run(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath']))?.trim();
    if (install) {
      const version = (await readFile(join(install, 'VC', 'Auxiliary', 'Build', 'Microsoft.VCToolsVersion.default.txt'), 'utf8').catch(() => ''))?.trim();
      const cl = version ? join(install, 'VC', 'Tools', 'MSVC', version, 'bin', 'Hostx64', 'x64', 'cl.exe') : null;
      const banner = cl && await exists(cl) ? await run(cl, []) : null;
      result.msvc = { toolset: version || null, banner: /Version\s+([\d.]+)\s+for\s+(\S+)/.exec(banner ?? '')?.slice(1, 3).join(' ') ?? null };
    }
  }
  const sdkRoot = join(programFilesX86, 'Windows Kits', '10', 'Include');
  const sdks = (await readdir(sdkRoot).catch(() => [])).filter((name) => /^10\.\d+\.\d+\.\d+$/.test(name)).sort((a, b) => a.split('.').map(Number).reduce((d, v, i) => d || v - b.split('.').map(Number)[i], 0));
  result.sdk = sdks.at(-1) ?? null;
  const perl = await findOnPath(['perl.exe'], environment.PATH ?? environment.Path);
  if (perl) result.perl = { path: perl, version: /\(v([\d.]+)\)/.exec(await run(perl, ['--version']) ?? '')?.[1] ?? null };
  const nasm = await findOnPath(['nasm.exe'], environment.PATH ?? environment.Path);
  result.nasm = nasm ? { present: true, version: /version\s+([\d.]+)/i.exec(await run(nasm, ['-v']) ?? '')?.[1] ?? null } : { present: false, version: null };
  return result;
}

/** Enabled openssl-src features & openssl-sys version from `cargo metadata`'s resolve graph. */
export function parseMetadataSelection(metadata) {
  const packages = new Map((metadata?.packages ?? []).map((entry) => [entry.id, entry]));
  const node = (metadata?.resolve?.nodes ?? []).find((entry) => packages.get(entry.id)?.name === 'openssl-src');
  const sys = (metadata?.resolve?.nodes ?? []).find((entry) => packages.get(entry.id)?.name === 'openssl-sys');
  if (!node) return null;
  return { features: [...(node.features ?? [])].sort(), opensslSysVersion: sys ? packages.get(sys.id).version : null };
}

export function renderHelperCrate({ opensslSrcVersion, features, ccVersion }) {
  const featureList = features.map((name) => `"${name}"`).join(', ');
  return {
    'Cargo.toml': `[package]\nname = "rk-openssl-producer"\nversion = "0.0.0"\nedition = "2021"\nbuild = "build.rs"\npublish = false\n\n[workspace]\n\n[build-dependencies]\nopenssl-src = { version = "=${opensslSrcVersion}", features = [${featureList}] }\n${ccVersion ? `cc = "=${ccVersion}"\n` : ''}`,
    'src/lib.rs': '',
    'build.rs': `use std::{env, fs, path::{Path, PathBuf}};
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() { copy_dir(&entry.path(), &to); } else { fs::copy(entry.path(), to).unwrap(); }
    }
}
fn main() {
    println!("cargo:rerun-if-env-changed=RK_OPENSSL_DEST");
    let dest = PathBuf::from(env::var("RK_OPENSSL_DEST").expect("RK_OPENSSL_DEST"));
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let host = env::var("HOST").unwrap();
    let artifacts = openssl_src::Build::new().out_dir(&out).target(&target).host(&host).build();
    copy_dir(artifacts.include_dir(), &dest.join("include"));
    copy_dir(artifacts.lib_dir(), &dest.join("lib"));
}
`
  };
}

const alive = (pid) => { try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; } };

export class NativeOpenSslCache {
  #root; #exec; #spawn; #now; #probe; #env; #verified = new Map(); #inflight = new Map(); #selections = new Map(); #toolchains = new Map();

  /** root: short directory inside the managed root (for example D:\.rightkit-managed\n). */
  constructor({ root, exec = execFile, spawn = nodeSpawn, now = Date.now, probe = probeWindowsToolchain, environment = process.env, producer = null } = {}) {
    if (typeof root !== 'string' || !root) throw new TypeError('native cache root is required');
    this.#root = root; this.#exec = exec; this.#spawn = spawn; this.#now = now; this.#probe = probe; this.#env = environment; this.producer = producer;
  }

  get root() { return this.#root; }
  entryPath(key16) { return join(this.#root, key16); }
  get enabled() { return this.#env[OPENSSL_OPT_OUT_ENV] !== '0'; }

  /** Resolve everything the consuming cargo run needs. Never throws: failures become `skipped`. */
  async prepare({ lockRoot, targetTriple, environment, argv = [], cargoPath, producerEnvironment }) {
    const skipped = (reason, extra = {}) => ({ status: 'skipped', skipped: reason, key: null, buildMs: null, path: null, environment: {}, ...extra });
    try {
      if (!this.enabled) return skipped('opt-out');
      const lockText = await readFile(join(lockRoot, 'Cargo.lock'), 'utf8').catch(() => null);
      if (lockText === null) return null;
      const detected = detectOpenSsl({ lockText, targetTriple });
      if (!detected.needed) return detected.reason === 'no-openssl-src' || detected.reason === 'unsupported-target' ? null : skipped(detected.reason);
      const overrides = findOverrides(environment, targetTriple);
      if (overrides.length) return skipped('user-override', { overrides });
      const selection = await this.#selection({ lockRoot, lockText, targetTriple, argv, cargoPath, producerEnvironment, detected });
      if (!selection) return skipped('metadata-unavailable');
      const toolchain = await this.#toolchain(producerEnvironment ?? environment);
      const derived = deriveKey({ opensslSrc: detected.opensslSrc, features: selection.features, targetTriple, toolchain, cc: detected.cc?.version ?? null });
      const entry = await this.ensure({ derived, detected, selection, toolchain, targetTriple, cargoPath, producerEnvironment });
      return { ...entry, environment: injectionEnvironment(targetTriple, entry.path), key: derived.key16 };
    } catch (error) {
      return skipped('producer-failed', { error: String(error?.message ?? error).slice(0, 500) });
    }
  }

  async #toolchain(environment) {
    const cacheKey = environment?.PATH ?? environment?.Path ?? '';
    if (!this.#toolchains.has(cacheKey)) this.#toolchains.set(cacheKey, await this.#probe({ environment }));
    return this.#toolchains.get(cacheKey);
  }

  async #selection({ lockRoot, lockText, targetTriple, argv, cargoPath, producerEnvironment, detected }) {
    const flags = [];
    for (let i = 0; i < argv.length; i += 1) {
      const arg = argv[i];
      if (arg === '--all-features' || arg === '--no-default-features') flags.push(arg);
      else if ((arg === '--features' || arg === '-F') && argv[i + 1]) { flags.push('--features', argv[i + 1]); i += 1; }
      else if (/^--features=/.test(arg)) flags.push(arg);
    }
    const id = createHash('sha256').update(JSON.stringify([lockText, targetTriple, flags])).digest('hex');
    if (this.#selections.has(id)) return this.#selections.get(id);
    let selection = null;
    for (const extra of [['--offline'], []]) {
      try {
        const { stdout } = await this.#exec(cargoPath, ['metadata', '--format-version', '1', '--locked', '--filter-platform', targetTriple, ...extra, ...flags], { cwd: lockRoot, env: producerEnvironment, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, timeout: 120_000, windowsHide: true });
        selection = parseMetadataSelection(JSON.parse(stdout));
        break;
      } catch { /* try the next mode */ }
    }
    if (selection) this.#selections.set(id, selection);
    return selection;
  }

  /** Verify an entry against its manifest. Full hashing once per broker lifetime, stat signature afterwards. */
  async verify(key16, expectedKey, { full = false } = {}) {
    const directory = this.entryPath(key16);
    const manifest = JSON.parse(await readFile(join(directory, 'manifest.json'), 'utf8'));
    if (manifest.key !== expectedKey || manifest.recipeVersion !== OPENSSL_RECIPE_VERSION) throw new Error('manifest key or recipe mismatch');
    const signature = [];
    for (const [relative, expected] of Object.entries(manifest.files)) {
      const path = join(directory, relative); const info = await stat(path);
      signature.push(`${relative}:${info.size}:${info.mtimeMs}`);
      if (info.size !== expected.size) throw new Error(`size mismatch: ${relative}`);
    }
    for (const lib of REQUIRED_LIBS) if (!manifest.files[lib]) throw new Error(`missing ${lib}`);
    const header = await readFile(join(directory, 'include', 'openssl', 'opensslv.h'), 'utf8').catch(() => '');
    if (!header.includes(String(manifest.expectedHeaderVersion))) throw new Error('opensslv.h version mismatch');
    const fingerprint = signature.join('|');
    if (full || this.#verified.get(key16) !== fingerprint) {
      for (const [relative, expected] of Object.entries(manifest.files)) if (await sha256File(join(directory, relative)) !== expected.sha256) throw new Error(`hash mismatch: ${relative}`);
      this.#verified.set(key16, fingerprint);
    }
    return manifest;
  }

  async #quarantine(key16, reason) {
    const target = join(this.#root, 'q', `${key16}-${this.#now()}`);
    await mkdir(join(this.#root, 'q'), { recursive: true });
    await writeFile(join(this.entryPath(key16), 'QUARANTINED'), reason).catch(() => {});
    await rename(this.entryPath(key16), target);
    this.#verified.delete(key16);
    return target;
  }

  async ensure({ derived, detected, selection, toolchain, targetTriple, cargoPath, producerEnvironment }) {
    const { key16, key } = derived; const path = this.entryPath(key16);
    const startedAt = this.#now();
    let recovered = null;
    if (await exists(path)) {
      try { await this.verify(key16, key); await utimes(path, new Date(), new Date()).catch(() => {}); return { status: 'hit', path, buildMs: 0, verifyMs: this.#now() - startedAt }; }
      catch (error) { recovered = `quarantined: ${error.message}`; await this.#quarantine(key16, recovered); }
    }
    if (!this.#inflight.has(key16)) this.#inflight.set(key16, this.#produce({ derived, detected, selection, toolchain, targetTriple, cargoPath, producerEnvironment }).finally(() => this.#inflight.delete(key16)));
    const produced = await this.#inflight.get(key16);
    return { ...produced, ...(recovered ? { recovered } : {}) };
  }

  async #lock(key16) {
    const directory = join(this.#root, 'l'); await mkdir(directory, { recursive: true });
    const lockPath = join(directory, `${key16}.lock`);
    for (;;) {
      try {
        const handle = await open(lockPath, 'wx');
        await handle.writeFile(JSON.stringify({ pid: process.pid, at: this.#now() })); await handle.close();
        return async () => { await rm(lockPath, { force: true }); };
      } catch (error) {
        if (error.code !== 'EEXIST') throw error;
        const holder = JSON.parse(await readFile(lockPath, 'utf8').catch(() => '{}'));
        const stale = !holder.pid || !alive(holder.pid) || this.#now() - (holder.at ?? 0) > 45 * 60_000;
        if (stale && holder.pid !== process.pid) { await rm(lockPath, { force: true }); continue; }
        if (await exists(this.entryPath(key16))) return null;
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
    }
  }

  async #produce({ derived, detected, selection, toolchain, targetTriple, cargoPath, producerEnvironment }) {
    const { key16, key } = derived; const path = this.entryPath(key16);
    const release = await this.#lock(key16);
    const startedAt = this.#now();
    if (release === null || await exists(path)) {
      try { await this.verify(key16, key); return { status: 'hit', path, buildMs: 0, verifyMs: this.#now() - startedAt }; } catch { /* fall through and rebuild */ }
      if (release === null) throw new Error('entry produced by another producer failed verification');
    }
    const work = join(this.#root, 'p', key16.slice(0, 8)); const staging = join(this.#root, 's', `${key16.slice(0, 8)}-${randomUUID().slice(0, 8)}`);
    try {
      await rm(work, { recursive: true, force: true }); await mkdir(join(work, 'h', 'src'), { recursive: true }); await mkdir(join(work, 'x'), { recursive: true }); await mkdir(staging, { recursive: true });
      const crate = renderHelperCrate({ opensslSrcVersion: detected.opensslSrc.version, features: selection.features, ccVersion: detected.cc?.version ?? null });
      for (const [name, text] of Object.entries(crate)) await writeFile(join(work, 'h', name), text);
      const env = { ...producerEnvironment };
      for (const name of Object.keys(env)) if (/^(RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_BUILD_RUSTC_WRAPPER|SCCACHE_.*|RIGHTKIT_PARENT_.*|CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(name)) delete env[name];
      Object.assign(env, { CARGO_TARGET_DIR: join(work, 't'), TMP: join(work, 'x'), TEMP: join(work, 'x'), TMPDIR: join(work, 'x'), RK_OPENSSL_DEST: staging });
      await this.#runProducer({ cargoPath, cwd: join(work, 'h'), env, targetTriple });
      const files = {}; for (const relative of await listFiles(staging)) files[relative] = { size: (await stat(join(staging, relative))).size, sha256: await sha256File(join(staging, relative)) };
      for (const lib of REQUIRED_LIBS) if (!files[lib]) throw new Error(`producer output lacks ${lib}`);
      const header = await readFile(join(staging, 'include', 'openssl', 'opensslv.h'), 'utf8').catch(() => '');
      const expectedHeaderVersion = detected.opensslSrc.version.split('+')[1] ?? detected.opensslSrc.version;
      if (!header.includes(expectedHeaderVersion)) throw new Error(`opensslv.h does not report ${expectedHeaderVersion}`);
      await writeFile(join(staging, 'manifest.json'), `${JSON.stringify({ schemaVersion: 1, recipeVersion: OPENSSL_RECIPE_VERSION, key, key16, expectedHeaderVersion, inputs: JSON.parse(derived.canonical), opensslSrc: detected.opensslSrc, opensslSysVersion: selection.opensslSysVersion, toolchain, targetTriple, producedAt: new Date(this.#now()).toISOString(), files }, null, 2)}\n`);
      await rename(staging, path);
      await this.verify(key16, key, { full: true });
      return { status: 'miss', path, buildMs: this.#now() - startedAt };
    } finally {
      await rm(staging, { recursive: true, force: true }).catch(() => {});
      await rm(work, { recursive: true, force: true }).catch(() => {});
      await release?.();
    }
  }

  async #runProducer({ cargoPath, cwd, env, targetTriple }) {
    if (this.producer) return this.producer({ cargoPath, cwd, env, targetTriple });
    for (const extra of [['--offline'], []]) {
      const outcome = await new Promise((resolve) => {
        const child = this.#spawn(cargoPath, ['build', '--quiet', '--target', targetTriple, ...extra], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
        let output = ''; child.stdout?.on('data', (c) => { output = (output + c).slice(-4000); }); child.stderr?.on('data', (c) => { output = (output + c).slice(-4000); });
        child.once('error', (error) => resolve({ code: -1, output: String(error) })); child.once('close', (code) => resolve({ code, output }));
      });
      if (outcome.code === 0) return;
      if (extra.length === 0) throw new Error(`OpenSSL producer failed (${outcome.code}): ${outcome.output.slice(-800)}`);
    }
  }

  /** Remove unpinned entries beyond `keepUnpinned` (LRU by mtime). Pinned keys are never evicted. */
  async gc({ pinned = new Set(), keepUnpinned = 2 } = {}) {
    const entries = [];
    for (const name of await readdir(this.#root).catch(() => [])) {
      if (!/^[0-9a-f]{16}$/.test(name)) continue;
      entries.push({ name, mtime: (await stat(join(this.#root, name))).mtimeMs });
    }
    const unpinned = entries.filter((entry) => !pinned.has(entry.name)).sort((a, b) => b.mtime - a.mtime);
    const evicted = [];
    for (const entry of unpinned.slice(keepUnpinned)) { await rm(join(this.#root, entry.name), { recursive: true, force: true }); this.#verified.delete(entry.name); evicted.push(entry.name); }
    return Object.freeze({ evicted, pinned: [...pinned] });
  }
}
