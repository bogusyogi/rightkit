// Bounded agent-action guard for heavy writes to the Mac internal drive (Orthic-Labs/rightkit#800).
//
// Host policy (storage lead, Mac): /Volumes/D is the ONLY agent volume. /Volumes/E is Adrian's personal volume and
// agent writes there are denied, as are writes to every other mount. No agent creates simulators: `simctl create`
// and `clone` are denied (use the shared iPhone 18 Pro). DerivedData lives at /Volumes/D/.xcode/DerivedData and run
// outputs in /Volumes/D/scratch. The agent volume list comes from host config: `RIGHTKIT_AGENT_VOLUMES`
// (path-delimited `:`), else the single volume /Volumes/D that disk-policy.json names for mac scratch and workspace
// roots. This module is PreToolUse feedback for the agent-policy write hook (ownership-write-hook.mjs calls
// heavyWriteHookDecision first). It runs on macOS only and does nothing elsewhere.
//
// What it does, and no more:
//   * `simctl create|clone` is denied. Other simctl subcommands pass unless --set names a protected location.
//   * `xcodebuild` build/test/archive needs `-derivedDataPath` on a verified agent volume; so do any -archivePath,
//     -resultBundlePath, -exportPath, -clonedSourcePackagesDirPath, -packageCachePath and the SYMROOT/OBJROOT/
//     DSTROOT-style overrides that are present. Read-only queries pass.
//   * Writes (redirects, tee, cp, mv, mkdir, touch, rsync, ditto, tar, unzip, curl, wget, dd of=, find -exec ...) and
//     Write/Edit/apply_patch targets are checked after cwd and realpath resolution. They are denied when they land in
//     the internal CoreSimulator Devices directory, the internal Library/Caches directory of a known app owner, the
//     RightKit shared/models default path, or a mount that is not an agent volume.
//   * There is NO textual prefilter: every command and path is parsed and every destination is resolved.
//   * There is no full shell emulator. A construct is either resolved exactly or REFUSED with a message that says how
//     to rewrite it ("split into separate commands", "use an absolute path"). Refusal is always acceptable; a false
//     allow never. An unknown expression or option is never accepted as a known-safe prefix: a word that STARTS with an
//     unevaluated expansion could be an option (`cp "$X" dst`), so it is refused; unknown option arities are refused too.
//
// "Agent volume" means: the destination contains no `..`; every existing ancestor is resolved with realpath (symlinks
// followed, a dangling symlink is refused); the result is /Volumes/<name>/... with /Volumes/<name> an agent volume;
// and both that mount and the nearest existing ancestor are on a device different from "/", "/System/Volumes/Data"
// and the home volume (st_dev). `/Volumes/Macintosh HD` (a symlink to "/"), `/Volumes/D-evil` (not a mount) and
// `/Volumes/E` (not an agent volume) all fail.
//
// Not a sandbox and not confinement: scripts, services, child processes, makefiles, IDEs and tools that choose their
// own locations escape every visible argument. Native write-location proof is still required (README).
// Nothing here changes SDK defaults, moves, deletes, resets or symlinks anything.
import { lstatSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { homedir } from 'node:os';
import { posix as path } from 'node:path';
import { fileURLToPath } from 'node:url';

export const HEAVY_WRITE_TAG = '[HOOK:rightkit-heavy-write:block]';
export const AGENT_VOLUME = '/Volumes/D';
export const SCRATCH_EXAMPLE = '/Volumes/D/scratch/<run>';
export const DERIVED_DATA = '/Volumes/D/.xcode/DerivedData';
export const HEAVY_WRITE_RULES = Object.freeze([
  Object.freeze({ id: 'simctl-no-simulators', summary: 'simctl create|clone is denied; use the shared iPhone 18 Pro' }),
  Object.freeze({ id: 'xcodebuild-derived-data', summary: 'xcodebuild build/test/archive needs -derivedDataPath on an agent volume (/Volumes/D)' }),
  Object.freeze({ id: 'xcodebuild-output', summary: 'xcodebuild -archivePath/-resultBundlePath/-exportPath/-clonedSourcePackagesDirPath/-packageCachePath and SYMROOT-style overrides, when present, must be on an agent volume' }),
  Object.freeze({ id: 'internal-heavy-path', summary: 'writes into ~/Library/Developer/CoreSimulator/Devices, ~/Library/Caches/<app owner> and the RightKit shared/models path are denied' }),
  Object.freeze({ id: 'non-agent-volume', summary: 'writes to any /Volumes/<name> other than an agent volume (so /Volumes/E) are denied' }),
  Object.freeze({ id: 'undeterminable-destination', summary: 'a construct whose destination cannot be resolved exactly is refused with a rewrite hint, never guessed' }),
]);

const DYN = Symbol('dynamic');
/** `env -i` removed this variable from the child's environment (unlike "not tracked", which means "inherited from the hook"). */
const UNSET = Symbol('unset');
/** Map keys (not variable names) that record "this HOME/XDG_CONFIG_HOME was set by a plain shell assignment and never exported". They live in `vars` so every Map copy carries them. */
const UNEXPORTED = '\0unexported:';
const GIT_PROBE_VARS = ['HOME', 'XDG_CONFIG_HOME'];
/** Map key: whether HOME/XDG_CONFIG_HOME is exported is UNKNOWN (conditional export, export -n, an unmodelled flag, a dynamic word). gitProbeEnv refuses it. */
const UNKNOWN_EXPORT = '\0unknown-export:';
const HEAVY_PROGRAMS = /simctl|xcodebuild/i;
const MAX_DEPTH = 6;

// Known app owners come from the packaged ownership-repositories.json (repository names), as the write hook does.
const FALLBACK_OWNERS = ['coderight', 'cutright', 'genright', 'heardright', 'mailright', 'rightsites', 'scraperight', 'viewright', 'voiceright', 'postright', 'cockpit', 'legion', 'workright', 'sellright', 'pulse', 'swiftright'];
const EXTRA_OWNERS = ['rightkit', 'rustright', 'swiftrightfactory', 'orthic labs', 'orthiclabs'];
export function loadAppOwners() {
  let names = FALLBACK_OWNERS;
  try {
    const file = fileURLToPath(new URL('./ownership-repositories.json', import.meta.url));
    const parsed = JSON.parse(readFileSync(file, 'utf8')).repositories.map((entry) => String(entry.repositoryIdentity).split('/').at(-1).toLowerCase()).filter(Boolean);
    if (parsed.length) names = parsed;
  } catch { /* packaged list unreadable: the built-in copy of the same names applies */ }
  return [...new Set([...names, ...EXTRA_OWNERS])];
}
let ownersCache = null;
const cachedOwners = () => { ownersCache ??= loadAppOwners(); return ownersCache; };
const ownerCacheName = (name, owners) => {
  const lower = name.toLowerCase();
  const bundle = lower.replace(/^(?:app|com)\.orthic-?labs\./, '');
  return owners.some((owner) => [lower, bundle].some((candidate) => candidate === owner || /^[-_.]/.test(candidate.slice(owner.length)) && candidate.startsWith(owner)))
    || /^(?:e2e|parakeet)_/.test(lower);
};

// ---------------------------------------------------------------------------------------------------------------
// Filesystem seam (real by default; tests inject a virtual one).
export const realFs = {
  // null when the entry does not exist; 'symlink' | 'dir' | 'file' otherwise; throws on any other error (fail closed).
  kind(target) {
    try {
      const info = lstatSync(target);
      return info.isSymbolicLink() ? 'symlink' : info.isDirectory() ? 'dir' : 'file';
    } catch (error) { if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') return null; throw error; }
  },
  realpath: (target) => realpathSync.native(target),
  device: (target) => { try { return statSync(target).dev; } catch { return undefined; } },
};

/** Resolve `absolute` through every existing ancestor. Returns { real, anchor } or { error }. `absolute` has no `..`. */
function resolveDestination(absolute, fs) {
  let probe = absolute; const tail = [];
  try {
    for (let depth = 0; depth < 256; depth += 1) {
      if (fs.kind(probe) !== null) {
        let anchor;
        try { anchor = fs.realpath(probe); } catch (error) { return { error: `${probe} cannot be resolved (${error?.code ?? error?.message}); a dangling symlink is refused` }; }
        return { real: path.join(anchor, ...tail), anchor };
      }
      if (probe === '/') return { error: 'the filesystem root cannot be read' };
      tail.unshift(path.basename(probe)); probe = path.dirname(probe);
    }
    return { error: 'path is too deep to resolve' };
  } catch (error) { return { error: `${probe} cannot be inspected (${error?.code ?? error?.message})` }; }
}

/** Exact (physical) resolution of a path that may contain ".." after a symlink: each ".." applies to the real directory. */
function physicalResolve(absolute, fs) {
  let resolved = '/'; let missing = false;
  try {
    for (const part of absolute.split('/').filter(Boolean)) {
      if (part === '.') continue;
      if (part === '..') { resolved = path.dirname(resolved); continue; }
      const next = path.join(resolved, part);
      if (!missing && fs.kind(next) !== null) resolved = fs.realpath(next); else { missing = true; resolved = next; }
    }
    return { real: resolved };
  } catch (error) { return { error: `${absolute} cannot be resolved (${error?.code ?? error?.message})` }; }
}

const within = (parent, child) => {
  const [base, target] = [parent, child].map((value) => value.toLowerCase());
  return target === base || target.startsWith(base === '/' ? '/' : `${base}/`);
};

function context(options = {}) {
  const home = options.home ?? process.env.HOME ?? homedir();
  const volumesRoot = options.volumesRoot ?? '/Volumes';
  const configured = options.agentVolumes ?? (process.env.RIGHTKIT_AGENT_VOLUMES ? process.env.RIGHTKIT_AGENT_VOLUMES.split(':').filter(Boolean) : [path.join(volumesRoot, 'D')]);
  return {
    home,
    fs: options.fs ?? realFs,
    volumesRoot,
    agentVolumes: configured.map((volume) => path.normalize(volume).replace(/\/+$/, '')),
    systemRoots: options.systemRoots ?? ['/', '/System/Volumes/Data'],
    env: options.env ?? process.env, // the hook's own environment (inherited GIT_* overrides)
    clock: options.clock ?? Date.now,
    gitRunner: options.gitRunner ?? defaultGitRunner, // seam: (args, {cwd, timeout, home, path}) => {status, stdout}
    gitDeadline: null, // set on the first git spawn: one 5 s budget per hook call
    owners: options.owners ?? cachedOwners(),
    cwd: 'cwd' in options ? options.cwd : process.cwd(), // an explicit null means "working directory unknown"
  };
}
const isAgentMount = (mount, ctx) => ctx.agentVolumes.some((volume) => volume.toLowerCase() === mount.toLowerCase());

function internalDevices(ctx) {
  const devices = new Set();
  const roots = [...ctx.systemRoots];
  const home = resolveDestination(path.normalize(ctx.home), ctx.fs);
  roots.push(home.error ? ctx.home : home.anchor);
  for (const root of roots) {
    const device = ctx.fs.device(root);
    if (device === undefined) return { error: `cannot stat ${root}` };
    devices.add(device);
  }
  return { devices };
}

/** Verified-agent-volume check for a resolved word. Returns { ok: true, real } or { ok: false, why, unknown }. */
function agentDestination(word, cwd, ctx) {
  const fail = (why, unknown = false) => ({ ok: false, why, unknown });
  if (word.dyn) return fail('the value depends on a shell expansion, glob or substitution the guard cannot evaluate', true);
  if (!word.v || word.v.includes('\0')) return fail('the value is empty', true);
  const absolute = word.v.startsWith('/') ? word.v : cwd ? path.join(cwd, word.v) : null;
  if (absolute === null) return fail('the value is relative and the working directory is not known', true);
  if (word.v.split('/').includes('..')) return fail(`"${word.v}" contains ".." traversal`);
  const resolved = resolveDestination(path.normalize(absolute), ctx.fs);
  if (resolved.error) return fail(resolved.error);
  const relative = path.relative(ctx.volumesRoot, resolved.real);
  if (!relative || relative === '..' || relative.startsWith('../') || path.isAbsolute(relative)) return fail(`it resolves to ${resolved.real}, which is not under ${ctx.volumesRoot}/<name>`);
  const mount = path.join(ctx.volumesRoot, relative.split('/')[0]);
  if (!isAgentMount(mount, ctx)) return fail(`${mount} is not an agent volume (agent writes are allowed only on ${ctx.agentVolumes.join(', ')})`);
  let mountKind;
  try { mountKind = ctx.fs.kind(mount); } catch (error) { return fail(`${mount} cannot be inspected (${error?.code ?? error?.message})`); }
  if (mountKind !== 'dir') return fail(`${mount} is not a mounted volume directory`);
  const internal = internalDevices(ctx);
  if (internal.error) return fail(internal.error);
  for (const probe of [mount, resolved.anchor]) {
    const device = ctx.fs.device(probe);
    if (device === undefined) return fail(`cannot stat ${probe}`);
    if (internal.devices.has(device)) return fail(`${probe} is on the internal (system or home) volume, not an external mount`);
  }
  return { ok: true, real: resolved.real };
}

// ---------------------------------------------------------------------------------------------------------------
// Protected locations (writes are denied when the resolved destination lands inside one).
function protectedRoots(ctx) {
  return [
    { id: 'CoreSimulator Devices', root: path.join(ctx.home, 'Library/Developer/CoreSimulator/Devices') },
    { id: 'RightKit shared/models', root: path.join(ctx.home, 'Library/Application Support/Orthic Labs/shared/models') },
  ];
}
/** null, or { rule, what } for a destination that is internal-heavy, on a non-agent volume, or cannot be resolved. */
function protectedHit(candidate, ctx) {
  const roots = protectedRoots(ctx);
  const homeVariants = [ctx.home];
  const realHome = resolveDestination(path.normalize(ctx.home), ctx.fs);
  if (!realHome.error && realHome.real !== ctx.home) homeVariants.push(realHome.real);
  const matches = (value) => {
    for (const root of roots) {
      const forms = [root.root];
      const resolvedRoot = resolveDestination(root.root, ctx.fs);
      if (!resolvedRoot.error) forms.push(resolvedRoot.real);
      if (forms.some((form) => within(form, value))) return { rule: 'internal-heavy-path', what: `internal ${root.id} (${root.root})` };
    }
    for (const home of homeVariants) {
      const base = path.join(home, 'Library/Caches');
      if (within(base, value) && value.length > base.length + 1) {
        const owner = value.slice(base.length + 1).split('/')[0];
        if (ownerCacheName(owner, ctx.owners)) return { rule: 'internal-heavy-path', what: `internal known app cache (${path.join(base, owner)})` };
      }
    }
    if (within(ctx.volumesRoot, value) && value.length > ctx.volumesRoot.length + 1) {
      const mount = path.join(ctx.volumesRoot, value.slice(ctx.volumesRoot.length + 1).split('/')[0]);
      if (!isAgentMount(mount, ctx)) return { rule: 'non-agent-volume', what: `${mount}, which is not an agent volume (only ${ctx.agentVolumes.join(', ')})` };
    }
    return null;
  };
  const lexical = path.normalize(candidate);
  if (candidate.split('/').includes('..')) {
    const physical = physicalResolve(candidate, ctx.fs); // ".." after a symlink means the real parent, not the lexical one
    if (physical.error) return { rule: 'undeterminable-destination', what: physical.error };
    return matches(physical.real) ?? matches(lexical);
  }
  const resolved = resolveDestination(lexical, ctx.fs);
  if (resolved.error) return { rule: 'undeterminable-destination', what: resolved.error };
  return matches(resolved.real);
}

// ---------------------------------------------------------------------------------------------------------------
// Shell tokenizer. Unlike the dependency tokenizer in ownership-write-hook.mjs it KEEPS redirect targets, shell
// variables and command substitutions, and the operator before and after every simple command, so the guard can see
// (or admit it cannot see) every destination and whether a `cd` is certain to have run.
function tokenize(text) {
  const segments = []; let seg = { words: [], redirects: [], nested: [], startOp: null, endOp: null };
  let parts = []; let buf = ''; let started = false; let glob = false; let redirect = null; const heredocs = [];
  const flushLiteral = () => { if (buf) { parts.push({ t: 'lit', v: buf }); buf = ''; } };
  const endWord = () => {
    flushLiteral();
    if (started) {
      const word = { parts, glob };
      if (redirect) { seg.redirects.push({ op: redirect, target: word }); redirect = null; } else seg.words.push(word);
    }
    parts = []; started = false; glob = false;
  };
  const endSegment = (op = null, marker = null) => {
    endWord(); redirect = null; seg.endOp = op; segments.push(seg);
    if (marker) segments.push({ marker });
    seg = { words: [], redirects: [], nested: [], startOp: marker ? null : op, endOp: null };
  };
  const dynamicPart = () => { flushLiteral(); parts.push({ t: 'dyn' }); started = true; };
  const matching = (from) => { // index of the ")" matching the "(" just before `from`
    let depth = 1;
    for (let i = from; i < text.length; i += 1) {
      const c = text[i];
      if (c === '\\') i += 1;
      else if (c === "'") { const end = text.indexOf("'", i + 1); if (end < 0) throw new Error('unclosed quote'); i = end; }
      else if (c === '"') { i += 1; while (i < text.length && text[i] !== '"') { if (text[i] === '\\') i += 1; i += 1; } }
      else if (c === '(') depth += 1;
      else if (c === ')' && (depth -= 1) === 0) return i;
    }
    throw new Error('unclosed substitution');
  };
  let at = 0;
  const dollar = () => { // text[at] === '$'; leaves `at` on the last consumed character
    const next = text[at + 1];
    if (next === '(') {
      if (text[at + 2] === '(') { const end = matching(at + 2); dynamicPart(); at = end; return; }
      const end = matching(at + 2); seg.nested.push(text.slice(at + 2, end)); dynamicPart(); at = end; return;
    }
    if (next === '{') {
      const end = text.indexOf('}', at + 2); if (end < 0) throw new Error('unclosed expansion');
      const name = text.slice(at + 2, end);
      if (/^[A-Za-z_]\w*$/.test(name)) { flushLiteral(); parts.push({ t: 'var', name }); started = true; } else dynamicPart();
      at = end; return;
    }
    const name = /^[A-Za-z_]\w*/.exec(text.slice(at + 1));
    if (name) { flushLiteral(); parts.push({ t: 'var', name: name[0] }); started = true; at += name[0].length; return; }
    if (next === "'") { const end = text.indexOf("'", at + 2); if (end < 0) throw new Error('unclosed quote'); dynamicPart(); at = end; return; }
    if (next !== undefined && /[\d?$!@*#-]/.test(next)) { dynamicPart(); at += 1; return; }
    buf += '$'; started = true;
  };
  const backtick = () => {
    let end = at + 1;
    while (end < text.length && text[end] !== '`') end += text[end] === '\\' ? 2 : 1;
    if (end >= text.length) throw new Error('unclosed backtick');
    seg.nested.push(text.slice(at + 1, end).replace(/\\`/g, '`')); dynamicPart(); at = end;
  };
  for (; at < text.length; at += 1) {
    const ch = text[at]; const next = text[at + 1];
    if (ch === '\\') { if (next === '\n') at += 1; else if (next !== undefined) { buf += next; started = true; at += 1; } continue; }
    if (ch === "'") { const end = text.indexOf("'", at + 1); if (end < 0) throw new Error('unclosed quote'); buf += text.slice(at + 1, end); started = true; at = end; continue; }
    if (ch === '"') {
      started = true; at += 1;
      while (at < text.length && text[at] !== '"') {
        const c = text[at];
        if (c === '\\' && text[at + 1] !== undefined && '$`"\\\n'.includes(text[at + 1])) { if (text[at + 1] !== '\n') buf += text[at + 1]; at += 2; continue; }
        if (c === '$') { dollar(); at += 1; continue; }
        if (c === '`') { backtick(); at += 1; continue; }
        buf += c; at += 1;
      }
      if (at >= text.length) throw new Error('unclosed quote');
      continue;
    }
    if (ch === '$') { dollar(); continue; }
    if (ch === '`') { backtick(); continue; }
    if (ch === '#' && !started) { while (at < text.length && text[at] !== '\n') at += 1; at -= 1; continue; }
    if (ch === '>' || ch === '<' || (ch === '&' && next === '>')) {
      if ((ch === '>' || ch === '<') && next === '(') { const end = matching(at + 2); seg.nested.push(text.slice(at + 2, end)); dynamicPart(); at = end; continue; }
      if (/^\d+$/.test(buf) && parts.length === 0 && started) { buf = ''; started = false; } else endWord();
      let op = ch;
      if (ch === '&') { at += 1; op = '&>'; if (text[at + 1] === '>') { at += 1; op = '&>>'; } }
      else if (ch === '>') { if (next === '>' || next === '|' || next === '&') { op += next; at += 1; } }
      else if (next === '<' && text[at + 2] === '<') { op = '<<<'; at += 2; }
      else if (next === '<') {
        at += 1; op = '<<'; if (text[at + 1] === '-') { at += 1; op = '<<-'; }
        let from = at + 1; while (/[ \t]/.test(text[from] ?? '')) from += 1;
        let to = from; while (to < text.length && !/[\s;|&<>()]/.test(text[to])) to += 1;
        heredocs.push({ delimiter: text.slice(from, to).replace(/["'\\]/g, ''), strip: op === '<<-' });
        at = to - 1; continue;
      } else if (next === '&' || next === '>') { op += next; at += 1; }
      redirect = op; continue;
    }
    if (ch === '\n') {
      endSegment('\n');
      for (const doc of heredocs.splice(0)) {
        let line = at + 1;
        while (line < text.length) {
          let end = text.indexOf('\n', line); if (end < 0) end = text.length;
          const body = text.slice(line, end); at = end;
          line = end + 1;
          if ((doc.strip ? body.replace(/^\t+/, '') : body) === doc.delimiter) break;
        }
      }
      continue;
    }
    if (ch === ';' || ch === '|' || ch === '&') {
      const double = next === ch || (ch === '|' && next === '&');
      const pair = double ? ch + next : ch;
      endSegment(pair === ';;' ? ';' : pair === '|&' ? '|' : pair);
      if (double) at += 1;
      continue;
    }
    if (ch === '(') { endSegment(null, 'open'); continue; }
    if (ch === ')') { endSegment(null, 'close'); continue; }
    if (/\s/.test(ch)) { endWord(); continue; }
    if (ch === '{' || ch === '}') {
      const bare = !started && (next === undefined || /[\s;&|)]/.test(next));
      if (!bare) glob = true;
    }
    if (ch === '*' || ch === '?' || ch === '[') glob = true;
    if (ch === '~' && !started) {
      if (next === undefined || /[\s/;&|)]/.test(next)) { parts.push({ t: 'var', name: 'HOME' }); started = true; continue; }
      glob = true;
    }
    buf += ch; started = true;
  }
  endSegment();
  return segments;
}

// ---------------------------------------------------------------------------------------------------------------
// Resolution of words against tracked shell variables and the tracked working directory.
function resolveWord(word, scope) {
  let v = ''; let dyn = Boolean(word.glob); let prefix = null; let partDyn = false;
  const cut = () => { if (prefix === null) prefix = v; };
  for (const part of word.parts) {
    if (part.t === 'lit') {
      v += part.v;
      if (word.glob && prefix === null && /[*?[{}~]/.test(part.v)) { prefix = v.slice(0, v.length - part.v.length) + part.v.slice(0, part.v.search(/[*?[{}~]/)); }
    } else if (part.t === 'var') {
      let value = scope.vars.get(part.name);
      if (value === undefined && scope.unknownAll) value = DYN; // an earlier eval/source/read may have set anything
      if (value === undefined && part.name === 'HOME') value = scope.home;
      if (value === undefined && part.name === 'PWD' && scope.cwd) value = scope.cwd;
      if (value === undefined || value === DYN) { dyn = true; partDyn = true; cut(); } else v += value === UNSET ? '' : value;
    } else { dyn = true; partDyn = true; cut(); }
  }
  if (word.glob && prefix === null) prefix = v;
  return { v, dyn, prefix: prefix === null ? v : prefix };
}
const assignmentOf = (word) => {
  const first = word.parts[0];
  if (first?.t !== 'lit') return null;
  const match = /^([A-Za-z_][A-Za-z0-9_]*)=/.exec(first.v);
  if (!match) return null;
  const rest = [{ t: 'lit', v: first.v.slice(match[0].length) }, ...word.parts.slice(1)].filter((part) => part.t !== 'lit' || part.v);
  return { name: match[1], word: { parts: rest, glob: word.glob } };
};
const variablesOf = (word) => word.parts.filter((part) => part.t === 'var').map((part) => part.name);
// Joins WITHOUT normalising: a ".." that follows a symlink must reach physicalResolve intact (realpath of each existing
// prefix first, then the ".." step), never be folded away lexically.
const absolutePath = (value, cwd) => (value.startsWith('/') ? value : cwd ? `${cwd === '/' ? '' : cwd.replace(/\/+$/, '')}/${value}` : null);
/** Real directory for a path, resolving each existing prefix before applying "..", or null when it cannot be resolved. */
const physicalDir = (absolute, ctx) => { const resolved = physicalResolve(absolute, ctx.fs); return resolved.error ? null : resolved.real; };
/**
 * The directory a shell `cd` lands in. Logical shells fold ".." lexically before chdir; `cd -P` and chdir(2) callers
 * (env -C, tool workdirs) resolve symlinks first. When the two disagree the directory is unknown (null).
 */
function cdTarget(value, cwd, ctx) {
  const absolute = absolutePath(value, cwd);
  if (!absolute) return null;
  const logical = physicalDir(path.normalize(absolute), ctx);
  const raw = physicalDir(absolute, ctx);
  return logical && raw && logical === raw ? raw : null;
}
/** The part of a resolved word after `offset` characters (an attached option value), keeping its dyn flag and static prefix. */
const sliceWord = (word, offset) => { const prefix = word.prefix ?? word.v; return { v: word.v.slice(offset), dyn: word.dyn, prefix: prefix.length > offset ? prefix.slice(offset) : '' }; };
const trimToDirectory = (prefix) => (prefix.endsWith('/') ? prefix : prefix.slice(0, prefix.lastIndexOf('/') + 1));

// ---------------------------------------------------------------------------------------------------------------
function violation(rule, why, required) { return { rule, why, required }; }
const AGENT_NOTE = `Agent volume: ${AGENT_VOLUME} only (run outputs in ${SCRATCH_EXAMPLE}, DerivedData in ${DERIVED_DATA}); /Volumes/E is Adrian's personal volume.`;
const requiredFor = {
  simctl: `Do not create simulators on this host: boot, install and launch on the shared iPhone 18 Pro instead. ${AGENT_NOTE}`,
  xcodebuild: `Pass -derivedDataPath ${DERIVED_DATA} (and any -archivePath / -resultBundlePath under ${SCRATCH_EXAMPLE}/). Read-only queries (-help, -version, -list, -showsdks, -showBuildSettings) are not affected. ${AGENT_NOTE}`,
  path: `Write on the agent volume instead: ${SCRATCH_EXAMPLE}/ for run output, ${DERIVED_DATA} for DerivedData, or the RightKit-configured agent store for caches and models. ${AGENT_NOTE}`,
};
const refuse = (why, hint) => violation('undeterminable-destination', why, `${hint} ${AGENT_NOTE}`);
const SPLIT = 'Split it into separate simple commands and use absolute paths or literal values (for example "cd /Volumes/D/x && cmd", or "VAR=value" on its own line before the command); the guard refuses constructs it cannot resolve exactly.';

const QUERY_FLAGS = new Set(['-help', '-usage', '-version', '-list', '-showsdks', '-showbuildsettings', '-showbuildsettingsforindex', '-showdestinations', '-showtestplans', '-checkfirstlaunchstatus']);
const XCODEBUILD_VALUE_FLAGS = new Set(['-project', '-workspace', '-scheme', '-target', '-configuration', '-sdk', '-arch', '-destination', '-destination-timeout', '-xcconfig', '-testplan', '-toolchain', '-jobs', '-scmprovider', '-exportoptionsplist', '-resultbundleversion', '-maximum-concurrent-test-device-destinations', '-maximum-concurrent-test-simulator-destinations', '-test-iterations', '-testlanguage', '-testregion', '-enablecodecoverage', '-parallel-testing-enabled', '-parallel-testing-worker-count', '-testproductspath', '-xctestrun', '-defaultpackageregistryurl', '-packagedependencyscmtoscm', '-authenticationkeypath', '-authenticationkeyid', '-authenticationkeyissuerid']);
const XCODEBUILD_BOOLEAN_FLAGS = new Set(['-quiet', '-verbose', '-json', '-allowprovisioningupdates', '-allowprovisioningdeviceregistration', '-skippackagepluginvalidation', '-skipmacrovalidation', '-disableautomaticpackageresolution', '-onlyusepackageversionsfromresolvedfile', '-parallelizetargets', '-dry-run', '-n', '-hideshellscriptenvironment', '-showbuildtimingsummary', '-exportarchive', '-exportnotarizedapp', '-skipunavailableactions', '-usepackagesupportbuiltinsctm']);
const OUTPUT_FLAGS = ['-derivedDataPath', '-archivePath', '-resultBundlePath', '-exportPath', '-clonedSourcePackagesDirPath', '-packageCachePath'];
const OUTPUT_SETTINGS = new Set(['SYMROOT', 'OBJROOT', 'DSTROOT', 'CONFIGURATION_BUILD_DIR', 'BUILD_DIR', 'BUILD_ROOT', 'CONFIGURATION_TEMP_DIR', 'TEMP_ROOT', 'SHARED_PRECOMPS_DIR', 'MODULE_CACHE_DIR']);
const SIMCTL_READ_ONLY = new Set(['list', 'help', 'getenv', 'listapps', 'get_app_container', 'pbpaste', 'status_bar', 'logverbose', 'spawn', 'diagnose', 'appinfo', 'notify_post']);

function simctlRule(args, scope, ctx) {
  let set = null; let sub = null; let setGiven = false;
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index];
    if (word.dyn && !word.prefix) return refuse('a simctl argument is built from a shell expansion, so the subcommand cannot be determined', SPLIT);
    if (word.v === '--set') { setGiven = true; set = args[index + 1] ?? null; index += 1; continue; }
    if (word.v.startsWith('--set=')) { setGiven = true; set = { v: word.v.slice(6), dyn: word.dyn }; continue; }
    if (word.v === '--profiles') { index += 1; continue; }
    if (word.v.startsWith('--profiles=')) continue;
    sub = word; break;
  }
  if (sub?.dyn) return refuse('the simctl subcommand is built from a shell expansion and cannot be determined', SPLIT);
  const name = sub?.v;
  if (!name) return null;
  if (name === 'create' || name === 'clone') return violation('simctl-no-simulators', `simctl ${name} creates a simulator (about 10 GB of runtime data on the internal drive was created that way); simulators are not allowed on this host`, requiredFor.simctl);
  if (setGiven && set && !SIMCTL_READ_ONLY.has(name)) {
    if (set.dyn) return refuse(`simctl ${name} --set is built from a shell expansion`, SPLIT);
    const target = absolutePath(set.v, scope.cwd);
    if (!target) return refuse(`simctl ${name} --set "${set.v}" is relative and the working directory is not known`, SPLIT);
    const hit = protectedHit(target, ctx);
    if (hit) return violation(hit.rule, `simctl ${name} --set targets ${hit.what}`, requiredFor.simctl);
  }
  return null;
}

function xcodebuildRule(args, scope, ctx) {
  const lower = args.map((word) => word.v.toLowerCase());
  if (lower.some((value) => QUERY_FLAGS.has(value))) return null;
  const values = new Map(); const positionals = []; const settings = []; let unknownFlag = false;
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index]; const key = word.v.toLowerCase();
    if (word.v.startsWith('-')) {
      const output = OUTPUT_FLAGS.find((flag) => flag.toLowerCase() === key);
      if (output) { const value = args[index + 1]; if (value) { values.set(output, [...(values.get(output) ?? []), value]); index += 1; } else values.set(output, [...(values.get(output) ?? []), { v: '', dyn: true, missing: true }]); continue; }
      if (XCODEBUILD_VALUE_FLAGS.has(key)) index += 1;
      else if (!XCODEBUILD_BOOLEAN_FLAGS.has(key)) unknownFlag = true; // arity unknown: never accepted as a known-safe prefix
      continue;
    }
    const setting = /^([A-Za-z_][A-Za-z0-9_]*)=/.exec(word.v);
    if (setting) { settings.push({ name: setting[1], word: { v: word.v.slice(setting[0].length), dyn: word.dyn } }); continue; }
    positionals.push(word);
  }
  const exporting = lower.includes('-exportarchive');
  const cleanOnly = positionals.length > 0 && positionals.every((word) => !word.dyn && word.v === 'clean') && !values.size && !unknownFlag && !settings.length;
  if (cleanOnly) return null;
  const check = (flag, word) => {
    if (word.missing) return refuse(`${flag} has no value that the guard can read`, SPLIT);
    const checked = agentDestination(word, scope.cwd, ctx);
    if (checked.ok) return null;
    const why = `${flag} ${word.dyn ? '(unevaluated)' : word.v}: ${checked.why}`;
    return checked.unknown ? refuse(why, `Use an absolute literal path such as ${DERIVED_DATA}.`) : violation(flag === '-derivedDataPath' ? 'xcodebuild-derived-data' : 'xcodebuild-output', why, requiredFor.xcodebuild);
  };
  if (exporting) {
    const exportPath = values.get('-exportPath');
    if (!exportPath) return violation('xcodebuild-output', 'xcodebuild -exportArchive without -exportPath exports into the current directory', requiredFor.xcodebuild);
    for (const word of exportPath) { const result = check('-exportPath', word); if (result) return result; }
    return null;
  }
  const derived = values.get('-derivedDataPath');
  if (!derived) return violation('xcodebuild-derived-data', 'xcodebuild without -derivedDataPath writes DerivedData under ~/Library/Developer/Xcode on the internal drive', requiredFor.xcodebuild);
  if (positionals.some((word) => word.v === 'archive') && !values.get('-archivePath')) return violation('xcodebuild-output', 'xcodebuild archive without -archivePath writes under ~/Library/Developer/Xcode/Archives on the internal drive', requiredFor.xcodebuild);
  for (const flag of OUTPUT_FLAGS) {
    for (const word of values.get(flag) ?? []) { const result = check(flag, word); if (result) return result; }
  }
  for (const setting of settings) {
    if (!OUTPUT_SETTINGS.has(setting.name)) continue;
    const result = check(setting.name, setting.word); if (result) return result;
  }
  return null;
}

// ---------------------------------------------------------------------------------------------------------------
// Option parsing. One engine handles short clusters with attached values (-o/path, -oPATH, -sSLo path), long options
// with "=" or a separate value, and unique-prefix abbreviations. Wrappers are strict (an unknown option means the
// program name cannot be located, so the command is refused); writers are permissive (unknown options are flags).
function parseOptions(args, spec) {
  const opts = []; const operands = []; const known = [...(spec.longArg ?? []), ...(spec.longFlag ?? [])];
  const shortFlag = spec.strict ? (spec.shortFlag ?? '') : spec.shortFlag; // a strict wrapper knows its flags; a writer guesses
  const take = (index) => (index < args.length ? args[index] : null);
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index];
    if (word.dyn && !word.prefix) { if (spec.strict) return { error: 'an option word is built from a shell expansion' }; operands.push(word); continue; }
    if (word.v === '--') { operands.push(...args.slice(index + 1)); break; }
    if (!word.v.startsWith('-') || word.v === '-') {
      if (spec.stopAtOperand) return { opts, operands: args.slice(index), index };
      operands.push(word); continue;
    }
    if (spec.multi && Object.hasOwn(spec.multi, word.v)) {
      if (spec.multi[word.v]) { const value = take(index + 1); if (!value) return { error: `${word.v} has no value` }; opts.push({ name: word.v.slice(1), value }); index += 1; } else opts.push({ name: word.v.slice(1), value: null });
      continue;
    }
    if (word.v.startsWith('--')) {
      const eq = word.v.indexOf('='); const name = word.v.slice(2, eq < 0 ? undefined : eq);
      const attached = eq < 0 ? null : sliceWord(word, eq + 1);
      const exact = known.includes(name) ? [name] : [];
      const candidates = exact.length ? exact : known.filter((candidate) => name && candidate.startsWith(name));
      if (!candidates.length) {
        // An unknown option is refused even as --name=value: knowing its arity does not tell what it means (it may name an
        // output file). A --no-X negation is accepted only when X itself is a known option.
        if (spec.allowNo && name.startsWith('no-') && known.includes(name.slice(3))) continue;
        if (spec.strict) return { error: `unknown option ${word.v}` };
        continue;
      }
      let consumed = false;
      for (const candidate of candidates) {
        if ((spec.longArg ?? []).includes(candidate)) {
          let value = attached;
          if (!value) { value = take(index + 1); if (!value) return { error: `${word.v} has no value` }; consumed = true; }
          opts.push({ name: candidate, value });
        } else opts.push({ name: candidate, value: null });
      }
      if (consumed) index += 1;
      continue;
    }
    let valued = false;
    for (let k = 1; k < word.v.length; k += 1) {
      const letter = word.v[k];
      if (word.dyn && k >= (word.prefix ?? '').length) return { error: 'an option cluster continues with an unresolved expansion' };
      if ((spec.shortArg ?? '').includes(letter)) {
        const rest = word.v.slice(k + 1);
        let value;
        if (word.dyn || k + 1 < word.v.length) value = sliceWord(word, k + 1); // an unresolved expansion right after the letter is an attached (dynamic) value
        else { value = take(index + 1); if (!value) return { error: `-${letter} has no value` }; index += 1; }
        opts.push({ name: letter, value });
        valued = true;
        break;
      }
      if (shortFlag === undefined || shortFlag.includes(letter) || (spec.numeric && /\d/.test(letter))) { opts.push({ name: letter, value: null }); continue; }
      if (spec.strict) return { error: `unknown option -${letter}` };
    }
    if (word.dyn && !valued) return { error: 'an option cluster ends with an unresolved expansion' };
  }
  return { opts, operands, index: args.length };
}

const WRAPPERS = {
  sudo: { shortArg: 'ugpCDrtTUR', shortFlag: 'AbEeHKklnPSVv', longArg: ['user', 'group', 'prompt', 'close-from', 'chdir', 'host', 'role', 'type', 'command-timeout', 'other-user', 'chroot'], longFlag: ['askpass', 'background', 'preserve-env', 'edit', 'set-home', 'non-interactive', 'stdin', 'reset-timestamp', 'remove-timestamp', 'list', 'validate', 'version', 'help', 'bell'], refuseShort: 'si', refuseLong: ['login', 'shell'], assignments: true },
  env: { shortArg: 'uCPa', shortFlag: 'i0v', longArg: ['unset', 'chdir', 'argv0'], longFlag: ['ignore-environment', 'null', 'debug'], refuseShort: 'S', refuseLong: ['split-string'], assignments: true },
  nice: { shortArg: 'n', shortFlag: '', numeric: true, longArg: ['adjustment'] },
  time: { shortArg: 'fo', shortFlag: 'alphvq', longArg: ['format', 'output'], longFlag: ['append', 'verbose', 'portability', 'quiet', 'help', 'version'], writes: ['o', 'output'] },
  timeout: { shortArg: 'sk', shortFlag: 'fpv', longArg: ['signal', 'kill-after'], longFlag: ['foreground', 'preserve-status', 'verbose'], skipOperands: 1 },
  nohup: { shortFlag: '' },
  command: { shortFlag: 'pvV', queryShort: 'vV' },
  builtin: { shortFlag: '' },
  exec: { shortArg: 'a', shortFlag: 'cl' },
  caffeinate: { shortArg: 'tw', shortFlag: 'dimsu' },
  arch: { multi: { '-arm64': 0, '-x86_64': 0, '-x86_64h': 0, '-i386': 0, '-arch': 1, '-c': 0, '-d': 1, '-e': 1, '-32': 0, '-64': 0 }, shortFlag: '' },
  stdbuf: { shortArg: 'ioe', longArg: ['input', 'output', 'error'] },
  xargs: { shortArg: 'nIPLdsEaJRS', shortFlag: '0rtpxo', longArg: ['max-args', 'max-procs', 'replace', 'max-lines', 'delimiter', 'arg-file', 'max-chars', 'eof'], longFlag: ['null', 'no-run-if-empty', 'verbose', 'interactive', 'open-tty', 'exit'] },
  setsid: { shortFlag: 'cfw', longFlag: ['ctty', 'fork', 'wait'] },
  unbuffer: { shortFlag: 'p' },
  xcrun: { multi: { '-sdk': 1, '-toolchain': 1, '-find': 0 }, shortFlag: 'vlnkrfh', longArg: ['sdk', 'toolchain'], longFlag: ['verbose', 'log', 'no-cache', 'kill-cache', 'run', 'find', 'help', 'version', 'show-sdk-path', 'show-sdk-version', 'show-sdk-build-version', 'show-sdk-platform-path', 'show-sdk-platform-version', 'show-toolchain-path'], queryShort: 'fh', queryLong: ['find', 'help', 'version', 'show-sdk-path', 'show-sdk-version', 'show-sdk-build-version', 'show-sdk-platform-path', 'show-sdk-platform-version', 'show-toolchain-path'] },
};
const SHELLS = new Set(['sh', 'bash', 'zsh', 'dash', 'ksh']);
const INTERPRETERS = new Set(['python', 'python3', 'node', 'ruby', 'perl', 'osascript', 'php', 'deno', 'bun']);
const KEYWORDS = new Set(['if', 'then', 'else', 'elif', 'do', 'while', 'until', '!', '{', '}', 'fi', 'done', 'esac', 'in']);
const REMOTE = new Set(['ssh', 'plink', 'mosh']);

// Ordinary writers. Option tables name the options that carry a destination or a value that is not an operand.
// Every table is STRICT: shortArg lists the single-letter options that take a value, shortFlag the ones that do not,
// longArg/longFlag the same for long options. An option in neither list is refused (its arity would be a guess, and a
// wrong guess either swallows a real destination or turns a value into one). `--name=value` for an unknown long option
// is accepted because the "=" fixes the arity. `allowNo` accepts rsync/wget style `--no-<flag>` negations.
const WRITERS = {
  cp: { dest: 'last', shortArg: 'tS', shortFlag: 'afHiLlnPpRrsTuvXxcdbNZ', longArg: ['target-directory', 'suffix'], longFlag: ['archive', 'force', 'interactive', 'link', 'no-clobber', 'no-dereference', 'preserve', 'recursive', 'symbolic-link', 'update', 'verbose', 'parents', 'backup', 'reflink', 'sparse', 'no-target-directory', 'one-file-system', 'dereference', 'remove-destination', 'strip-trailing-slashes', 'attributes-only', 'copy-contents', 'debug'], destOpts: ['t', 'target-directory'] },
  mv: { dest: 'last', shortArg: 'tS', shortFlag: 'finvhTubx', longArg: ['target-directory', 'suffix'], longFlag: ['force', 'interactive', 'no-clobber', 'verbose', 'update', 'backup', 'no-target-directory', 'strip-trailing-slashes', 'debug'], destOpts: ['t', 'target-directory'] },
  ln: { dest: 'last', shortArg: 'tS', shortFlag: 'sfinvFhLPrTdb', longArg: ['target-directory', 'suffix'], longFlag: ['symbolic', 'force', 'interactive', 'no-dereference', 'verbose', 'logical', 'physical', 'relative', 'no-target-directory', 'directory', 'backup'], destOpts: ['t', 'target-directory'] },
  install: { dest: 'last', shortArg: 'Bfgmot', shortFlag: 'bCcdMpsUvDT', longArg: ['target-directory', 'mode', 'owner', 'group', 'suffix'], longFlag: ['directory', 'compare', 'preserve-timestamps', 'strip', 'verbose', 'create-leading-dirs', 'no-target-directory', 'debug', 'backup'], destOpts: ['t', 'target-directory'] },
  ditto: { dest: 'last', shortArg: '', shortFlag: 'cxkzjVvXhm', longArg: ['arch', 'bom', 'zlibCompressionLevel'], longFlag: ['rsrc', 'norsrc', 'extattr', 'noextattr', 'noqtn', 'nocache', 'hfsCompression', 'nohfsCompression', 'preserveHFSCompression', 'keepParent', 'sequesterRsrc', 'noacl'], multi: { '-rsrc': 0, '-norsrc': 0, '-V': 0 } },
  scp: { dest: 'last', shortArg: 'cDFiJloPS', shortFlag: '346ABCOpqRrsTvXx', longArg: [], longFlag: [] },
  rsync: {
    dest: 'last', shortArg: 'eBTfM@', shortFlag: 'abcCdDEFghHiIkKlLmnopPqrRsStuUvVWxXyz0468',
    longArg: ['rsh', 'rsync-path', 'port', 'address', 'sockopts', 'bwlimit', 'block-size', 'max-delete', 'max-size', 'min-size', 'modify-window', 'temp-dir', 'backup-dir', 'suffix', 'compare-dest', 'copy-dest', 'link-dest', 'partial-dir', 'log-file', 'log-file-format', 'out-format', 'exclude', 'exclude-from', 'include', 'include-from', 'filter', 'files-from', 'chmod', 'chown', 'usermap', 'groupmap', 'checksum-choice', 'compress-choice', 'compress-level', 'skip-compress', 'timeout', 'contimeout', 'stop-after', 'stop-at', 'info', 'debug', 'iconv', 'read-batch', 'write-batch', 'only-write-batch', 'remote-option', 'protocol', 'password-file', 'early-input', 'outbuf', 'max-alloc', 'checksum-seed', 'bind'],
    longFlag: ['archive', 'verbose', 'quiet', 'checksum', 'recursive', 'relative', 'backup', 'update', 'inplace', 'append', 'append-verify', 'dirs', 'links', 'copy-links', 'copy-unsafe-links', 'safe-links', 'munge-links', 'copy-dirlinks', 'keep-dirlinks', 'hard-links', 'perms', 'executability', 'acls', 'xattrs', 'owner', 'group', 'devices', 'specials', 'times', 'atimes', 'crtimes', 'omit-dir-times', 'omit-link-times', 'super', 'fake-super', 'sparse', 'preallocate', 'dry-run', 'whole-file', 'one-file-system', 'existing', 'ignore-existing', 'remove-source-files', 'delete', 'delete-before', 'delete-during', 'delete-delay', 'delete-after', 'delete-excluded', 'delete-missing-args', 'ignore-missing-args', 'force', 'ignore-errors', 'prune-empty-dirs', 'numeric-ids', 'compress', 'progress', 'partial', 'itemize-changes', 'human-readable', 'stats', 'list-only', 'no-motd', 'from0', 'cvs-exclude', 'ignore-times', 'size-only', 'mkpath', 'fuzzy', 'copy-devices', 'write-devices', 'blocking-io', 'implied-dirs', 'delay-updates', 'help', 'version', 'secluded-args', 'protect-args', 'stderr', 'trust-sender', 'copy-as', 'daemon', 'detach', 'no-detach', 'server', 'sender', 'ipv4', 'ipv6'],
    allowNo: true, destOpts: ['T', 'temp-dir', 'log-file'],
  },
  mkdir: { dest: 'all', shortArg: 'm', shortFlag: 'pv', longArg: ['mode', 'context'], longFlag: ['parents', 'verbose'] },
  touch: { dest: 'all', shortArg: 'trdA', shortFlag: 'acfhm', longArg: ['reference', 'date', 'time'], longFlag: ['no-create', 'no-dereference'] },
  tee: { dest: 'all', shortArg: '', shortFlag: 'aip', longArg: [], longFlag: ['append', 'ignore-interrupts', 'output-error'] },
  truncate: { dest: 'all', shortArg: 'sr', shortFlag: 'co', longArg: ['size', 'reference'], longFlag: ['no-create', 'io-blocks'] },
  dd: { dest: 'dd', shortArg: '', shortFlag: '', longArg: [], longFlag: ['help', 'version'] },
  unzip: { dest: 'unzip', shortArg: 'dxP', shortFlag: 'aAbcCfjklLMnopqsStTuUvVXz2', longArg: [], longFlag: [], destOpts: ['d'] },
  curl: {
    dest: 'curl', shortArg: 'AbcCdDeEFHKmoPQrtTuUwxXyYz', shortFlag: '0123456aBfgGhiIjJklLMnNOpqRsSvVZ#',
    longArg: ['abstract-unix-socket', 'alt-svc', 'aws-sigv4', 'cacert', 'capath', 'cert', 'cert-type', 'ciphers', 'config', 'connect-timeout', 'connect-to', 'continue-at', 'cookie', 'cookie-jar', 'create-file-mode', 'crlfile', 'curves', 'data', 'data-ascii', 'data-binary', 'data-raw', 'data-urlencode', 'delegation', 'dns-interface', 'dns-ipv4-addr', 'dns-ipv6-addr', 'dns-servers', 'doh-url', 'dump-header', 'egd-file', 'engine', 'etag-compare', 'etag-save', 'expect100-timeout', 'form', 'form-string', 'ftp-account', 'ftp-alternative-to-user', 'ftp-method', 'ftp-port', 'happy-eyeballs-timeout-ms', 'haproxy-clientip', 'header', 'hostpubmd5', 'hostpubsha256', 'hsts', 'interface', 'ipfs-gateway', 'json', 'keepalive-time', 'key', 'key-type', 'krb', 'libcurl', 'limit-rate', 'local-port', 'login-options', 'mail-auth', 'mail-from', 'mail-rcpt', 'max-filesize', 'max-redirs', 'max-time', 'netrc-file', 'noproxy', 'oauth2-bearer', 'output', 'output-dir', 'parallel-max', 'pass', 'pinnedpubkey', 'preproxy', 'proto', 'proto-default', 'proto-redir', 'proxy', 'proxy-cacert', 'proxy-capath', 'proxy-cert', 'proxy-header', 'proxy-key', 'proxy-pass', 'proxy-user', 'proxy1.0', 'pubkey', 'quote', 'random-file', 'range', 'rate', 'referer', 'request', 'request-target', 'resolve', 'retry', 'retry-delay', 'retry-max-time', 'sasl-authzid', 'service-name', 'socks4', 'socks4a', 'socks5', 'socks5-hostname', 'speed-limit', 'speed-time', 'stderr', 'tftp-blksize', 'time-cond', 'tls-max', 'tls13-ciphers', 'tlsauthtype', 'tlspassword', 'tlsuser', 'trace', 'trace-ascii', 'unix-socket', 'upload-file', 'url', 'url-query', 'user', 'user-agent', 'variable', 'vlan-priority', 'write-out'],
    longFlag: ['anyauth', 'append', 'basic', 'buffer', 'ca-native', 'compressed', 'compressed-ssh', 'create-dirs', 'crlf', 'digest', 'disable', 'disable-eprt', 'disable-epsv', 'disallow-username-in-url', 'fail', 'fail-early', 'fail-with-body', 'false-start', 'get', 'globoff', 'head', 'help', 'http0.9', 'http1.0', 'http1.1', 'http2', 'http2-prior-knowledge', 'http3', 'http3-only', 'include', 'insecure', 'ipv4', 'ipv6', 'junk-session-cookies', 'list-only', 'location', 'location-trusted', 'manual', 'negotiate', 'netrc', 'netrc-optional', 'next', 'no-alpn', 'no-buffer', 'no-keepalive', 'no-npn', 'no-progress-meter', 'no-sessionid', 'ntlm', 'ntlm-wb', 'parallel', 'parallel-immediate', 'path-as-is', 'post301', 'post302', 'post303', 'progress-bar', 'proxy-anyauth', 'proxy-basic', 'proxy-digest', 'proxy-insecure', 'proxy-negotiate', 'proxy-ntlm', 'proxytunnel', 'raw', 'remote-header-name', 'remote-name', 'remote-name-all', 'remote-time', 'retry-all-errors', 'retry-connrefused', 'sasl-ir', 'show-error', 'silent', 'ssl', 'ssl-allow-beast', 'ssl-no-revoke', 'ssl-reqd', 'styled-output', 'suppress-connect-headers', 'tcp-fastopen', 'tcp-nodelay', 'tftp-no-options', 'tlsv1', 'tlsv1.0', 'tlsv1.1', 'tlsv1.2', 'tlsv1.3', 'tr-encoding', 'trace-time', 'use-ascii', 'verbose', 'version', 'xattr', 'fail-with-body'],
    allowNo: true, destOpts: ['o', 'output', 'D', 'dump-header', 'c', 'cookie-jar', 'trace', 'trace-ascii', 'stderr', 'libcurl', 'etag-save', 'hsts', 'alt-svc'],
  },
  wget: {
    dest: 'wget', shortArg: 'oaetTwQOPiBlARDIXUY', shortFlag: 'VhbdqvcNSrkKEpHmxFL46', multi: { '-nv': 0, '-nc': 0, '-nd': 0, '-nH': 0, '-np': 0, '-nr': 0 },
    longArg: ['output-document', 'directory-prefix', 'output-file', 'append-output', 'input-file', 'base', 'user-agent', 'header', 'tries', 'timeout', 'wait', 'waitretry', 'limit-rate', 'quota', 'accept', 'reject', 'domains', 'exclude-domains', 'include-directories', 'exclude-directories', 'level', 'cut-dirs', 'user', 'password', 'http-user', 'http-password', 'load-cookies', 'save-cookies', 'post-data', 'post-file', 'ca-certificate', 'certificate', 'private-key', 'referer', 'execute', 'bind-address', 'dns-timeout', 'connect-timeout', 'read-timeout', 'progress', 'report-speed', 'config', 'restrict-file-names', 'prefer-family', 'hsts-file', 'warc-file'],
    longFlag: ['continue', 'timestamping', 'no-clobber', 'no-verbose', 'quiet', 'verbose', 'recursive', 'mirror', 'no-parent', 'no-host-directories', 'no-directories', 'force-directories', 'span-hosts', 'convert-links', 'page-requisites', 'adjust-extension', 'content-disposition', 'no-check-certificate', 'spider', 'background', 'version', 'help', 'no-cache', 'no-cookies', 'keep-session-cookies', 'inet4-only', 'inet6-only', 'show-progress', 'trust-server-names', 'https-only', 'no-http-keep-alive', 'unlink', 'delete-after', 'retry-connrefused', 'server-response', 'content-on-error', 'debug', 'no-config', 'random-wait'],
    allowNo: true, destOpts: ['O', 'output-document', 'o', 'output-file', 'a', 'append-output', 'save-cookies', 'hsts-file', 'warc-file'],
  },
};

function programName(word) { return path.basename(word.v.replace(/\\/g, '/')).replace(/\.exe$/i, ''); }

/**
 * A write destination. G1: every unresolved destination is refused, including one with a safe-looking prefix (a
 * "/Volumes/D/" head followed by any unresolved part). A static head that already lands in a protected place is denied
 * with its own rule first. `prefixOnly` (a dynamic program name) checks the static head and never refuses.
 */
function protectedWrite(word, scope, ctx, via, prefixOnly = false) {
  if (word.dyn) {
    const head = trimToDirectory(word.prefix ?? '');
    const absoluteHead = head ? absolutePath(head, scope.cwd) : null;
    const headHit = absoluteHead ? protectedHit(absoluteHead, ctx) : null;
    if (headHit && headHit.rule !== 'undeterminable-destination') return violation(headHit.rule, `${via} writes to ${headHit.what}`, requiredFor.path);
    if (prefixOnly) return null;
    return refuse(`${via}: the destination is built from a shell expansion, glob or substitution that cannot be resolved, and an unresolved part can land anywhere even after a safe-looking prefix`, 'Use a literal absolute path (no variables, command substitution, glob or brace expansion) on the agent volume.');
  }
  const text = word.v;
  if (!text) return null;
  const absolute = absolutePath(text, scope.cwd);
  if (!absolute) return refuse(`${via}: the destination "${text}" is relative and the working directory is not known (an earlier cd may not have run)`, 'Use an absolute path, or put the cd in the same && chain as the command ("cd /Volumes/D/x && cmd").');
  const hit = protectedHit(absolute, ctx);
  return hit ? violation(hit.rule, `${via} writes to ${hit.what}`, hit.rule === 'undeterminable-destination' ? `${SPLIT} ${AGENT_NOTE}` : requiredFor.path) : null;
}
const cwdWord = (scope) => (scope.cwd ? { v: scope.cwd, dyn: false } : null);

/** Tar mode and destinations. Create/append/update/delete write -f; extract writes the -C directories (or the cwd). */
function tarRule(args, scope, ctx) {
  // -h is --dereference (BSD and GNU), a no-value flag; help is only --help or -?. A help word never skips an output option.
  if (args.some((word) => ['--help', '--version', '--usage', '-?'].includes(word.v)) && !args.some((word) => /^--(?:i|l)/.test(word.v))) return null;
  const letters = { flag: 'vkmnOoPpqSUwzjJZyaBlLhHdGiVWRFE0123456789', arg: 'fCbITXsN' };
  const longMode = { create: 'c', extract: 'x', get: 'x', list: 't', append: 'r', update: 'u', delete: 'd', catenate: 'A', concatenate: 'A', diff: 'x?', compare: 'x?' };
  const longArg = ['file', 'directory', 'exclude', 'include', 'files-from', 'exclude-from', 'transform', 'xform', 'strip-components', 'format', 'use-compress-program', 'blocking-factor', 'newer', 'newer-mtime', 'options', 'owner', 'group', 'mode', 'uid', 'gid', 'uname', 'gname', 'label', 'index-file', 'listed-incremental', 'to-command', 'rsh-command'];
  const longFlag = ['verbose', 'keep-old-files', 'overwrite', 'preserve-permissions', 'same-permissions', 'gzip', 'gunzip', 'bzip2', 'xz', 'lzma', 'zstd', 'compress', 'uncompress', 'no-same-owner', 'no-same-permissions', 'numeric-owner', 'one-file-system', 'dereference', 'null', 'totals', 'no-recursion', 'exclude-vcs', 'absolute-names', 'keep-newer-files', 'touch', 'to-stdout', 'wildcards', 'no-wildcards', 'anchored', 'help', 'version', 'unlink-first', 'sparse', 'ignore-failed-read', 'safe-links', 'no-xattrs', 'xattrs', 'acls', 'no-acls', 'mac-metadata', 'no-mac-metadata', 'fast-read', 'clear-nochange-fflags', 'chroot', 'nodump', 'read-full-blocks', 'lzop', 'lz4', 'lrzip', 'lzip', 'posix', 'ignore-zeros'];
  const known = [...Object.keys(longMode), ...longArg, ...longFlag];
  const modes = new Set(); const files = []; const dirs = []; const outputs = []; let unknown = null;
  const list = args.slice();
  const first = list[0];
  if (first && !first.dyn && first.v && !first.v.startsWith('-')) list[0] = { ...first, v: `-${first.v}` }; // old style: "tar cf a.tar ..."
  for (let index = 0; index < list.length && !unknown; index += 1) {
    const word = list[index];
    if (word.dyn && !word.prefix) { if (index === 0 || word.v === '') { unknown = 'an option or mode word is built from a shell expansion'; } continue; }
    if (word.v === '--') break;
    if (!word.v.startsWith('-') || word.v === '-') continue;
    if (word.v.startsWith('--')) {
      const eq = word.v.indexOf('='); const name = word.v.slice(2, eq < 0 ? undefined : eq);
      const exact = known.includes(name) ? [name] : known.filter((candidate) => name && candidate.startsWith(name));
      if (!exact.length) { unknown = `unknown tar option ${word.v}`; break; }
      for (const candidate of exact) {
        if (candidate in longMode) modes.add(longMode[candidate]);
        if (longArg.includes(candidate)) {
          const value = eq >= 0 ? sliceWord(word, eq + 1) : list[index + 1];
          if (eq < 0) index += 1;
          if (!value) { unknown = `${word.v} has no value`; break; }
          if (candidate === 'file') files.push(value);
          if (candidate === 'directory') dirs.push(value);
          if (candidate === 'index-file' || candidate === 'listed-incremental') outputs.push({ word: value, via: `tar --${candidate}` });
        }
      }
      continue;
    }
    let valued = false;
    for (let k = 1; k < word.v.length; k += 1) {
      const letter = word.v[k];
      if ('cxtruA'.includes(letter)) { modes.add(letter); continue; }
      if (letters.arg.includes(letter)) {
        const attached = word.dyn || k + 1 < word.v.length;
        const value = attached ? sliceWord(word, k + 1) : list[index + 1];
        if (!attached) index += 1;
        if (!value) { unknown = `-${letter} has no value`; break; }
        if (letter === 'f') files.push(value);
        if (letter === 'C') dirs.push(value);
        valued = true;
        break;
      }
      if (letters.flag.includes(letter)) continue;
      unknown = `unknown tar option -${letter}`; break;
    }
    if (word.dyn && !valued && !unknown) unknown = 'an option cluster ends with an unresolved expansion';
  }
  if (unknown) return refuse(`tar: ${unknown}`, 'State the mode (-c, -x or -t) and options explicitly: "tar -cf /Volumes/D/scratch/a.tar -C <read dir> ." or "tar -xf a.tar -C /Volumes/D/scratch/x".');
  modes.delete('x?');
  if (modes.size !== 1) return refuse(`tar: the mode (create, extract or list) ${modes.size ? 'is ambiguous' : 'cannot be determined'}`, 'State exactly one mode explicitly: "tar -cf <archive> -C <read dir> ." (create), "tar -xf <archive> -C <dir>" (extract) or "tar -tf <archive>" (list).');
  const mode = [...modes][0];
  const check = (word, via) => protectedWrite(word, scope, ctx, via);
  // --index-file and --listed-incremental name files tar WRITES, in every mode, so they are checked before any read-only return.
  for (const output of outputs) { const found = check(output.word, output.via); if (found) return found; }
  if (mode === 't') return null;
  if (mode === 'x') {
    if (!dirs.length) { // extraction lands in the working directory
      const here = cwdWord(scope);
      return here ? check(here, 'tar -x (no -C, extracts into the working directory)') : refuse('tar -x without -C extracts into a working directory that is not known', 'Use "tar -xf <archive> -C <absolute dir>".');
    }
    let base = scope.cwd;
    for (const dir of dirs) {
      if (dir.dyn) return refuse('tar -C is built from a shell expansion', SPLIT);
      const resolved = absolutePath(dir.v, base);
      if (!resolved) return refuse('tar -C is relative and the working directory is not known', 'Use "tar -xf <archive> -C <absolute dir>".');
      base = physicalDir(resolved, ctx); // chdir(2): resolve symlinks of each existing prefix before any ".."
      if (!base) return refuse(`tar -C ${dir.v} cannot be resolved`, 'Use "tar -xf <archive> -C <absolute dir that exists>".');
      const found = check({ v: base, dyn: false }, 'tar -x -C'); if (found) return found;
    }
    return null;
  }
  // create / append / update / delete / catenate: -C is a READ directory, -f is the write destination.
  for (const file of files) {
    if (!file.dyn && file.v === '-') continue;
    if (dirs.length && !file.dyn && !file.v.startsWith('/')) return refuse('tar -f is a relative archive path combined with -C; implementations differ on which directory it is relative to', 'Use an absolute archive path: "tar -cf /Volumes/D/scratch/a.tar -C <dir> .".');
    const found = check(file, 'tar -f (archive)'); if (found) return found;
  }
  return null;
}

function writerRule(program, args, scope, ctx) {
  const spec = WRITERS[program];
  const parsed = parseOptions(args, { ...spec, strict: true });
  // An option whose arity is not in the table is refused: guessing it either swallows a real destination (a flag read as
  // taking a value) or turns a value into a destination operand.
  if (parsed.error) return refuse(`${program}: ${parsed.error}; the guard does not know that option's arity, so it cannot tell which word is the destination`, `Remove the option, or split the command so the destination is an explicit absolute path given with options the guard models.`);
  const targets = [];
  const has = (...names) => parsed.opts.some((opt) => names.includes(opt.name));
  const valuesOf = (...names) => parsed.opts.filter((opt) => names.includes(opt.name) && opt.value).map((opt) => opt.value);
  const collect = (names) => { for (const opt of parsed.opts) if (names.includes(opt.name) && opt.value) targets.push({ word: opt.value, via: `${program} -${opt.name.length > 1 ? '-' : ''}${opt.name}` }); };
  const operands = parsed.operands;
  const last = operands.at(-1);
  const remote = (word) => ['rsync', 'scp'].includes(program) && /^[^/]*:/.test(word.v) && !word.v.startsWith('/');
  if (spec.destOpts) collect(spec.destOpts);
  const hasDirectory = has('t', 'target-directory');
  if (spec.dest === 'last') {
    if (program === 'install' && has('d', 'directory')) {
      for (const word of operands) targets.push({ word, via: 'install -d' }); // every operand is a directory to create
    } else if (program === 'ln') {
      // ln: with -t DIR the destination is DIR; with 2+ operands it is the last (a directory means <dir>/<basename> of each
      // source, which lies inside it); with ONE operand the link is created at <cwd>/<basename(target)>.
      if (!hasDirectory) {
        if (operands.length >= 2) targets.push({ word: last, via: 'ln' });
        else if (operands.length === 1) {
          const target = operands[0];
          if (target.dyn) return refuse('ln: the single operand is built from a shell expansion, so the link name <cwd>/<basename> cannot be resolved', SPLIT);
          if (!scope.cwd) return refuse('ln with one operand creates the link in the working directory, which is not known (an earlier cd may not have run)', 'Give ln an explicit absolute link name as its second operand.');
          targets.push({ word: { v: `${scope.cwd.replace(/\/+$/, '')}/${path.basename(target.v.replace(/\/+$/, '')) || target.v}`, dyn: false }, via: 'ln (single operand: link in the working directory)' });
        }
      }
    } else if (operands.length >= 2 && !remote(last) && !(last.stdin && hasDirectory)) {
      // With -t/--target-directory the last operand is a source in GNU tools but the destination in BSD ones (which have
      // no -t); both are checked, so a protected last operand is denied either way.
      targets.push({ word: last, via: program });
    }
  }
  if (spec.dest === 'all') for (const word of operands) targets.push({ word, via: program });
  if (spec.dest === 'dd') for (const word of args) if (word.v.startsWith('of=')) targets.push({ word: sliceWord(word, 3), via: 'dd of=' });
  if (spec.dest === 'unzip') {
    const readOnly = parsed.opts.some((opt) => ['l', 'p', 't', 'v', 'Z'].includes(opt.name));
    if (!readOnly && !parsed.opts.some((opt) => opt.name === 'd')) { const here = cwdWord(scope); if (here) targets.push({ word: here, via: 'unzip (no -d, extracts into the working directory)' }); else return refuse('unzip without -d extracts into a working directory that is not known', 'Use "unzip <archive> -d <absolute dir>".'); }
  }
  if (spec.dest === 'curl') {
    // -K/--config can set any output option, so it is refused. -w/--write-out can write a file through %output{}.
    if (has('K', 'config')) return refuse('curl -K/--config: the config file can set any output path', 'Pass the options on the command line instead of a config file.');
    for (const word of valuesOf('w', 'write-out')) if (word.dyn || /%output\s*\{/.test(word.v)) return refuse('curl --write-out may write a file through %output{...} (or is built from a shell expansion)', 'Use a literal --write-out format without %output{}.');
    // Implicit output: -O/--remote-name/--remote-name-all/-J write into --output-dir, else the working directory.
    const dir = valuesOf('output-dir')[0];
    if (dir) targets.push({ word: dir, via: 'curl --output-dir' });
    if (has('O', 'remote-name', 'remote-name-all', 'J') && !dir) {
      const here = cwdWord(scope);
      if (here) targets.push({ word: here, via: 'curl -O/--remote-name-all (writes into the working directory)' });
      else return refuse('curl -O/--remote-name-all writes into a working directory that is not known', 'Use "curl -o <absolute file>" or --output-dir with an absolute path.');
    }
  }
  if (spec.dest === 'wget') {
    const document = parsed.opts.find((opt) => opt.name === 'O' || opt.name === 'output-document')?.value;
    const prefix = parsed.opts.find((opt) => opt.name === 'P' || opt.name === 'directory-prefix')?.value;
    if (prefix) targets.push({ word: prefix, via: 'wget -P' });
    if (!document && !prefix && operands.length) { const here = cwdWord(scope); if (here) targets.push({ word: here, via: 'wget (writes into the working directory)' }); else return refuse('wget writes into a working directory that is not known', 'Use "wget -O <absolute file>".'); }
  }
  if (program === 'rsync') {
    // --write-batch=F and --only-write-batch=F write F and F.sh. --backup-dir and --partial-dir resolve against the
    // DESTINATION root (receiver side); --temp-dir is checked against both the cwd and the destination root.
    for (const word of valuesOf('write-batch', 'only-write-batch')) {
      targets.push({ word, via: 'rsync --write-batch' });
      targets.push({ word: { v: `${word.v}.sh`, dyn: word.dyn, prefix: word.prefix }, via: 'rsync --write-batch (the .sh companion)' });
    }
    const destination = operands.length >= 2 ? last : null;
    const destRoot = destination && !remote(destination) && !destination.dyn ? absolutePath(destination.v, scope.cwd) : null;
    for (const [names, label] of [[['backup-dir'], 'rsync --backup-dir'], [['partial-dir'], 'rsync --partial-dir'], [['T', 'temp-dir'], 'rsync --temp-dir']]) {
      for (const word of valuesOf(...names)) {
        if (word.dyn || word.v.startsWith('/')) { if (!names.includes('T')) targets.push({ word, via: label }); continue; } // -T/--temp-dir are already collected as destOpts
        if (destination && remote(destination)) continue; // the remote side
        if (!destRoot) return refuse(`${label} ${word.v} is relative to the destination root, which cannot be resolved`, 'Use an absolute --backup-dir/--partial-dir/--temp-dir path and a literal destination.');
        targets.push({ word: { v: `${destRoot.replace(/\/+$/, '')}/${word.v}`, dyn: false }, via: `${label} (relative to the destination root)` });
      }
    }
  }
  for (const target of targets) {
    if (target.word.v === '-' && ['wget', 'curl', 'dd'].includes(program)) continue; // stdout
    const found = protectedWrite({ ...target.word, prefix: target.word.prefix ?? target.word.v }, scope, ctx, target.via);
    if (found) return found;
  }
  return null;
}

// ---------------------------------------------------------------------------------------------------------------
function analyzeText(text, ctx, state, depth) {
  const heavyText = HEAVY_PROGRAMS.test(text);
  if (depth > MAX_DEPTH) return refuse('shell nesting is too deep to analyze', SPLIT);
  let segments;
  try { segments = tokenize(text); } catch (error) {
    return refuse(`the command cannot be parsed (${error.message}), so its write destinations cannot be determined`, SPLIT);
  }
  const stack = [];
  for (let index = 0; index < segments.length; index += 1) {
    const seg = segments[index];
    if (seg.marker === 'open') {
      // "name ()" opens a function definition: nothing in its body runs at definition time, so treat the rest as conditional.
      const body = segments[index + 1]; const before = segments[index - 1];
      if (body && !body.marker && !body.words.length && segments[index + 2]?.marker === 'close' && before && !before.marker && before.words.length) state.compound = true;
      stack.push({ vars: new Map(state.vars), cwd: state.cwd, chainCd: state.chainCd }); state.chainCd = false; continue;
    }
    if (seg.marker === 'close') { const saved = stack.pop(); if (saved) { state.vars = saved.vars; state.cwd = saved.cwd; state.chainCd = saved.chainCd; } continue; }
    const found = analyzeSegment(seg, ctx, state, depth, heavyText);
    if (found) return found;
    // A `cd` is certain to have run only inside the same && chain; once the chain ends the working directory is unknown.
    if (seg.endOp !== '&&' && state.chainCd) { state.cwd = null; state.chainCd = false; }
  }
  return null;
}

/** A copy of the shell state for a subshell, command substitution or child shell. */
const forkState = (state, vars = state.vars, cwd = state.cwd) => ({ vars: new Map(vars), cwd, home: state.home, chainCd: false, compound: state.compound, unknownAll: state.unknownAll });
/** After eval/source/read: any variable may have changed, so no earlier value is trusted. */
function poison(state) { state.vars = new Map(); state.unknownAll = true; state.chainCd = false; state.cwd = null; }
/** An assignment certainly runs only at the start of a chain outside any compound command, function body, pipeline or background job. */
const certainRun = (ops, state) => !state.compound && [null, ';', '\n'].includes(ops?.startOp ?? null) && [null, ';', '\n', '&&', '||'].includes(ops?.endOp ?? null);
function assignVar(state, ops, name, value) { state.vars.set(name, certainRun(ops, state) ? value : DYN); }
/** Export state of `name` before an assignment: true (in the environment git inherits), false (a shell-only variable) or 'unknown'. Only HOME/XDG_CONFIG_HOME are tracked. */
function exportState(state, ctx, name) {
  if (!GIT_PROBE_VARS.includes(name)) return true;
  if (state.vars.has(UNKNOWN_EXPORT + name)) return 'unknown';
  const current = state.vars.get(name);
  return !state.vars.has(UNEXPORTED + name) && current !== UNSET && (typeof current === 'string' || Object.hasOwn(ctx.env, name));
}
function markExport(state, name, exported) {
  if (!GIT_PROBE_VARS.includes(name)) return;
  state.vars.delete(UNEXPORTED + name); state.vars.delete(UNKNOWN_EXPORT + name);
  if (exported === false) state.vars.set(UNEXPORTED + name, true);
  else if (exported === 'unknown') state.vars.set(UNKNOWN_EXPORT + name, true);
}
/**
 * export/declare/typeset/readonly/local. Only the modelled literal forms change export state, and an EXPORT only counts when
 * it certainly runs: `export NAME[=literal]` and `declare|typeset -x NAME[=literal]` export; `declare|typeset|readonly|local
 * NAME[=literal]` without a flag do not. Any other flag (-n, -f, -p, -fx, +x, a dynamic word) or a dynamic name leaves the
 * export state of the named HOME/XDG_CONFIG_HOME (or of both, for a dynamic name) UNKNOWN, which the probe refuses.
 */
function declareBuiltin(program, rest, state, scope, ctx) {
  const certain = certainRun(scope.ops, state);
  const dynamicTarget = (word) => word.dyn && !/^[A-Za-z_][A-Za-z0-9_]*=/.test(word.v);
  const flags = rest.filter((word) => !word.dyn && /^[-+]./.test(word.v)).map((word) => word.v);
  const mode = program === 'export' ? (flags.length ? 'unknown' : 'export')
    : (program === 'declare' || program === 'typeset') ? (flags.length === 0 ? 'plain' : flags.every((flag) => flag === '-x') ? 'export' : 'unknown')
      : (flags.length ? 'unknown' : 'plain');
  if (rest.some(dynamicTarget)) for (const name of GIT_PROBE_VARS) markExport(state, name, 'unknown');
  for (const word of rest) {
    if (dynamicTarget(word) || (!word.dyn && /^[-+]./.test(word.v))) continue;
    const match = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(word.v);
    const name = match ? match[1] : /^[A-Za-z_][A-Za-z0-9_]*$/.test(word.v) ? word.v : null;
    if (!name) continue;
    const before = exportState(state, ctx, name);
    if (match) assignVar(state, scope.ops, name, word.dyn ? DYN : match[2]);
    markExport(state, name, mode === 'unknown' ? 'unknown' : mode === 'plain' ? before : certain || before === true ? true : 'unknown');
  }
}
const rawLiteral = (word) => (word && !word.glob && word.parts.length === 1 && word.parts[0].t === 'lit' ? word.parts[0].v : null);
const COMPOUND_KEYWORDS = new Set(['if', 'then', 'else', 'elif', 'do', 'while', 'until', '{', '}', 'fi', 'done', 'esac', 'in']);

function analyzeSegment(seg, ctx, state, depth, heavyText) {
  for (const inner of seg.nested) {
    const found = analyzeText(inner, ctx, forkState(state), depth + 1);
    if (found) return found;
  }
  // Leading control keywords (`then X=1`, `do cmd`) are stripped BEFORE assignments are read, and make every later
  // assignment conditional (the body may not run).
  let words = seg.words;
  while (words.length && (rawLiteral(words[0]) === '!' || COMPOUND_KEYWORDS.has(rawLiteral(words[0])))) {
    if (rawLiteral(words[0]) !== '!') state.compound = true;
    words = words.slice(1);
  }
  const ops = { startOp: seg.startOp, endOp: seg.endOp };
  // POSIX: the words of a simple command are expanded BEFORE its prefix assignments take effect; the assignments reach
  // only the child's environment. Everything below resolves with `pre` (the shell's values); `child` adds the assignments.
  const pre = { vars: new Map(state.vars), cwd: state.cwd, home: state.home, unknownAll: state.unknownAll, ops };
  const assigned = [];
  while (words.length && assignmentOf(words[0])) {
    const { name, word } = assignmentOf(words[0]);
    if (assigned.some(([earlier]) => variablesOf(word).includes(earlier))) return refuse(`the assignment ${name}= uses a variable assigned earlier in the same command, and shells differ on which value it sees`, SPLIT);
    const resolved = resolveWord(word, pre);
    assigned.push([name, resolved.dyn ? DYN : resolved.v]); words = words.slice(1);
  }
  const assignedNames = assigned.map(([name]) => name);
  for (const redirect of seg.redirects) {
    if (!/^(?:>|>>|>\||&>|&>>|>&|<>)$/.test(redirect.op)) continue;
    if (assignedNames.length && variablesOf(redirect.target).some((name) => assignedNames.includes(name))) return refuse('a redirect target uses a variable assigned in the same command (the shell expands it before the assignment applies)', SPLIT);
    const target = resolveWord(redirect.target, pre);
    if (/&/.test(redirect.op) && /^(?:\d+|-)$/.test(target.v)) continue;
    const found = protectedWrite(target, pre, ctx, `the redirect "${redirect.op} ${target.dyn ? '(unevaluated)' : target.v}"`);
    if (found) return found;
  }
  if (!words.length) { for (const [name, value] of assigned) { const was = exportState(state, ctx, name); assignVar(state, ops, name, value); markExport(state, name, was); } return null; }
  if (assignedNames.length && words.some((word) => variablesOf(word).some((name) => assignedNames.includes(name)))) {
    return refuse('a command word uses a variable assigned in the same command (the shell expands it before the assignment applies, so it sees the previous value)', SPLIT);
  }
  const child = { vars: new Map(pre.vars), cwd: pre.cwd, home: pre.home, unknownAll: pre.unknownAll, ops: pre.ops };
  for (const [name, value] of assigned) { child.vars.set(name, value); markExport(child, name, true); } // a prefix assignment reaches the child's environment
  const args = words.map((word) => resolveWord(word, pre));
  return analyzeCommand(args, ctx, state, child, depth, heavyText);
}

function wrapperParse(program, rest) {
  const spec = WRAPPERS[program];
  const parsed = parseOptions(rest, { ...spec, strict: true, stopAtOperand: true });
  if (parsed.error) return { error: parsed.error };
  for (const opt of parsed.opts) {
    if (spec.refuseShort && opt.name.length === 1 && spec.refuseShort.includes(opt.name)) return { error: `-${opt.name} runs its operand through a shell` };
    if (spec.refuseLong?.includes(opt.name)) return { error: `--${opt.name} runs its operand through a shell` };
  }
  let operands = parsed.operands;
  if (spec.skipOperands) operands = operands.slice(spec.skipOperands);
  const query = parsed.opts.some((opt) => (opt.name.length === 1 ? spec.queryShort?.includes(opt.name) : spec.queryLong?.includes(opt.name)));
  return { opts: parsed.opts, operands, query };
}

function shellScript(rest) {
  for (let index = 0; index < rest.length; index += 1) {
    const word = rest[index];
    if (word.dyn && !word.prefix) return { error: true };
    if (word.v === '--') return { script: null };
    if (word.v.startsWith('--')) { if (['--rcfile', '--init-file'].includes(word.v)) index += 1; continue; }
    if (/^[-+][A-Za-z]+$/.test(word.v)) {
      if (word.v[0] === '-' && word.v.includes('c')) return { script: rest[index + 1] ?? null };
      if (/[oO]/.test(word.v)) index += 1;
      continue;
    }
    break;
  }
  return { script: null };
}

// ---------------------------------------------------------------------------------------------------------------
// find, sed -i and git: commands that write (or hand a destination to another command) in ways the writer tables
// do not describe.

/** `find` start paths and expression. Leading -H/-L/-P/-E/-X/-d/-s/-x are options; -f PATH is a start path (BSD). */
function findStarts(rest) {
  const starts = []; const options = []; let index = 0;
  for (; index < rest.length; index += 1) {
    const word = rest[index];
    if (['-H', '-L', '-P', '-E', '-X', '-d', '-s', '-x'].includes(word.v)) { options.push(word.v); continue; }
    if (word.v === '-f' && rest[index + 1]) { starts.push(rest[index + 1]); index += 1; continue; }
    if (word.v.startsWith('-') || word.v === '(' || word.v === '!') break;
    starts.push(word);
  }
  return { starts, options, expression: rest.slice(index) };
}

function findRule(rest, ctx, state, scope, depth, heavyText) {
  const { starts, options, expression } = findStarts(rest);
  // -L and -H follow symlinks (the last of -H/-L/-P wins); -follow always does.
  const follows = ['-H', '-L'].includes(options.filter((option) => ['-H', '-L', '-P'].includes(option)).at(-1)) || expression.some((word) => word.v === '-follow');
  const startWords = starts.length ? starts : (scope.cwd ? [{ v: scope.cwd, dyn: false, prefix: scope.cwd }] : [{ v: '', dyn: true, prefix: '' }]);
  for (let index = 0; index < expression.length; index += 1) {
    const word = expression[index];
    if (word.v === '-delete') {
      if (follows) return refuse('find -delete combined with -L, -H or -follow: deletion would follow symlinks out of the start path that the guard checked', 'Use find -P (the default) with -delete, or list the paths and delete them in separate commands with absolute paths on /Volumes/D.');
      for (const start of startWords) { const found = protectedWrite({ ...start, prefix: start.prefix ?? start.v }, scope, ctx, 'find -delete'); if (found) return found; }
    } else if (['-fprint', '-fprint0', '-fprintf', '-fls'].includes(word.v) && expression[index + 1]) {
      const found = protectedWrite(expression[index + 1], scope, ctx, `find ${word.v}`); if (found) return found;
    } else if (word.v === '-execdir' || word.v === '-okdir') {
      // The nested command runs in the directory of each match, so neither its cwd nor its relative paths can be resolved.
      return refuse(`find ${word.v} runs the command in the directory of each match, whose location the guard cannot resolve`, 'Use find -exec with absolute destination paths, or collect the paths and act on them in separate commands.');
    } else if (word.v === '-exec' || word.v === '-ok') {
      let end = index + 1; while (end < expression.length && !(expression[end].v === ';' || expression[end].v === '+')) end += 1;
      // "{}" is whatever find matched: it is always an unresolved word (a start root does not bound it: -L follows a child
      // symlink to another volume), so a nested writer whose destination contains it is refused by the unresolved-destination rule.
      // Each nested command runs in a FORKED state and scope (copies of cwd, env and variables): a nested `env -C`, cd or
      // assignment must never change what a sibling or later command sees.
      const nestedState = forkState(state, scope.vars, scope.cwd);
      const nestedScope = { ...scope, vars: new Map(scope.vars), cwd: scope.cwd };
      const found = analyzeCommand(expression.slice(index + 1, end), ctx, nestedState, nestedScope, depth + 1, heavyText);
      if (found) return found;
      index = end;
    }
  }
  return null;
}

const SED_LONG_ARG = ['expression', 'line-length'];
const SED_LONG_FLAG = ['regexp-extended', 'quiet', 'silent', 'separate', 'null-data', 'zero-terminated', 'posix', 'debug', 'sandbox', 'follow-symlinks', 'binary', 'unbuffered', 'help', 'version', 'in-place'];
/**
 * Read a LITERAL sed script and report the files it writes: `w FILE`, `W FILE` and `s/x/y/w FILE` (the file name runs to
 * the end of the line). The `e` command and the `s///e` flag execute commands and are refused, as is anything the reader
 * cannot parse with certainty (an unknown flag, an unterminated pattern, an odd command): the caller refuses it.
 * Returns { files } or { error }.
 */
function sedScriptEffects(text) {
  const files = []; const n = text.length; let i = 0;
  const fail = (why) => ({ error: why });
  const readDelimited = (from, delimiter) => { // index just after the closing delimiter, or -1
    for (let at = from; at < n; at += 1) {
      if (text[at] === '\\') { at += 1; continue; }
      if (text[at] === delimiter) return at + 1;
      if (text[at] === '\n' && delimiter !== '\n') return -1;
    }
    return -1;
  };
  const restOfLine = (from) => { let end = text.indexOf('\n', from); if (end < 0) end = n; return { value: text.slice(from, end).replace(/^[ \t]+/, ''), end }; };
  while (i < n) {
    const c = text[i];
    if (/[\s;{}!]/.test(c)) { i += 1; continue; }
    if (c === '#') { i = restOfLine(i).end; continue; }
    // address: line numbers, $, first~step, addr+N, addr,addr, /regex/I, \cregexc
    if (/[0-9$,~+]/.test(c)) { i += 1; continue; }
    if (c === '/' || c === '\\') {
      const delimiter = c === '/' ? '/' : text[i + 1];
      if (!delimiter || delimiter === '\n' || delimiter === '\\') return fail('an address delimiter cannot be read');
      const end = readDelimited(c === '/' ? i + 1 : i + 2, delimiter);
      if (end < 0) return fail('an address regular expression is not terminated');
      i = end; while (/[IM]/.test(text[i] ?? '')) i += 1;
      continue;
    }
    if (c === 's') {
      const delimiter = text[i + 1];
      if (!delimiter || /[\s\\;]/.test(delimiter)) return fail('the s command delimiter cannot be read');
      const pattern = readDelimited(i + 2, delimiter); if (pattern < 0) return fail('an s command pattern is not terminated');
      const replacement = readDelimited(pattern, delimiter); if (replacement < 0) return fail('an s command replacement is not terminated');
      i = replacement;
      for (;;) {
        const flag = text[i];
        if (flag === undefined || /[\s;}]/.test(flag)) break;
        if (flag === 'w') { const { value, end } = restOfLine(i + 1); if (!value) return fail('an s///w has no file name'); files.push(value); i = end; break; }
        if (flag === 'e') return fail('the s///e flag executes commands');
        if (!/[gpiImM0-9]/.test(flag)) return fail(`the s command flag "${flag}" is not understood`);
        i += 1;
      }
      continue;
    }
    if (c === 'y') {
      const delimiter = text[i + 1];
      if (!delimiter || /[\s\\;]/.test(delimiter)) return fail('the y command delimiter cannot be read');
      const first = readDelimited(i + 2, delimiter); const second = first < 0 ? -1 : readDelimited(first, delimiter);
      if (second < 0) return fail('a y command is not terminated');
      i = second; continue;
    }
    if (c === 'w' || c === 'W') { const { value, end } = restOfLine(i + 1); if (!value) return fail(`the ${c} command has no file name`); files.push(value); i = end; continue; }
    if (c === 'e' || c === 'E') return fail('the e command executes commands');
    if (c === 'r' || c === 'R' || c === 'a' || c === 'i' || c === 'c') { // read file / text: to the end of the line (text may continue after a backslash-newline)
      let at = i + 1; for (;;) { const line = restOfLine(at); if (line.value.endsWith('\\') && line.end < n) { at = line.end + 1; continue; } i = line.end; break; }
      continue;
    }
    if (c === ':' || c === 'b' || c === 't' || c === 'T') { while (i < n && text[i] !== '\n' && text[i] !== ';') i += 1; continue; }
    if ('dDgGhHlnNpPqQxz='.includes(c)) { i += 1; continue; }
    return fail(`the command "${c}" is not understood`);
  }
  return { files };
}
/**
 * sed. BSD sed (`sed`): `-i EXT` takes the NEXT word as the backup suffix (possibly empty), `-iEXT` attaches it. GNU sed
 * (`gsed`): `-i` and `-iSUFFIX` only attach, `--in-place[=SUFFIX]`. -i edits each file operand in place (a destination).
 * A literal script is read: every w/W FILE and s///w FILE is a destination; e and s///e are refused. A script that is not
 * literal (an expansion), -f (the script is unseen), an unknown option, a BSD suffix word that looks like a script or option (`sed -i -e ...`, `sed -i 's/a/b/' f`), and an
 * in-place edit with no file operand.
 */
function sedRule(program, args, scope, ctx) {
  const gnu = program === 'gsed';
  const operands = []; const scripts = []; let inPlace = false; let scriptGiven = false;
  const refuseSed = (why) => refuse(`${program}: ${why}`, `Use ${gnu ? 'sed -i or -iSUFFIX' : "sed -i '' (or -i .bak)"} with a literal -e SCRIPT or script operand and absolute file paths, or split the command.`);
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index];
    if (word.dyn && !word.prefix) return refuseSed('an argument starts with an unevaluated expansion and could be an option');
    if (word.v === '--') { operands.push(...args.slice(index + 1)); break; }
    if (!word.v.startsWith('-') || word.v === '-') { operands.push(word); continue; }
    if (word.v.startsWith('--')) {
      const eq = word.v.indexOf('='); const name = word.v.slice(2, eq < 0 ? undefined : eq);
      const known = [...SED_LONG_ARG, ...SED_LONG_FLAG, 'file'];
      const matches = known.includes(name) ? [name] : known.filter((candidate) => name && candidate.startsWith(name));
      if (!matches.length) return refuseSed(`unknown option ${word.v}`);
      for (const match of matches) {
        if (match === 'file') return refuseSed('-f/--file reads a script that may contain w commands the guard cannot see');
        if (match === 'in-place') inPlace = true;
        if (match === 'expression') { scriptGiven = true; const value = eq >= 0 ? sliceWord(word, eq + 1) : args[index + 1]; if (eq < 0) index += 1; if (!value) return refuseSed('--expression has no value'); scripts.push(value); }
        else if (match === 'line-length' && eq < 0) index += 1;
      }
      continue;
    }
    let valued = false;
    for (let k = 1; k < word.v.length; k += 1) {
      const letter = word.v[k];
      if (letter === 'i') {
        inPlace = true; valued = true;
        if (word.dyn || k + 1 < word.v.length) break; // -iSUFFIX attached (BSD and GNU)
        if (!gnu) { // BSD: the next word is the backup suffix, even when empty
          const suffix = args[index + 1];
          if (!suffix) return refuseSed('-i has no backup suffix word');
          if (suffix.dyn || /[\s/\\]/.test(suffix.v) || suffix.v.startsWith('-')) return refuseSed(`BSD sed would take "${suffix.dyn ? '(unevaluated)' : suffix.v}" as the backup suffix, so the script and file positions are ambiguous`);
          index += 1;
        }
        break;
      }
      if (letter === 'e' || letter === 'f' || letter === 'l') {
        valued = true;
        if (letter === 'f') return refuseSed('-f/--file reads a script that may contain w commands the guard cannot see');
        const attached = word.dyn || k + 1 < word.v.length ? sliceWord(word, k + 1) : null;
        if (letter === 'e') { scriptGiven = true; const value = attached ?? args[index + 1]; if (!attached) index += 1; if (!value) return refuseSed('-e has no script'); scripts.push(value); } else if (!attached) index += 1;
        break;
      }
      if ('nEraszu'.includes(letter)) continue;
      return refuseSed(`unknown option -${letter}`);
    }
    if (word.dyn && !valued) return refuseSed('an option cluster ends with an unresolved expansion');
  }
  const scriptWords = [...scripts];
  if (!scriptGiven && operands.length) scriptWords.push(operands[0]);
  for (const text of scriptWords) {
    // A script can write through w/W and s///w, and run commands through e and s///e, so a script that is not a literal is refused.
    if (text.dyn) return refuseSed('the script is built from a shell expansion (or comes from outside the command line), and a script can write files through w/W or s///w');
    const effects = sedScriptEffects(text.v);
    if (effects.error) return refuseSed(`${effects.error}; the guard refuses a script it cannot read with certainty`);
    for (const written of effects.files) { const hit = protectedWrite({ v: written, dyn: false, prefix: written }, scope, ctx, 'sed w/W or s///w'); if (hit) return hit; }
  }
  if (!inPlace) return null;
  const files = scriptGiven ? operands : operands.slice(1);
  if (!files.length) return refuseSed('-i has no file operand after the script');
  for (const file of files) {
    if (file.v === '-' && !file.dyn) return refuseSed('-i cannot edit standard input');
    const found = protectedWrite({ ...file, prefix: file.prefix ?? file.v }, scope, ctx, 'sed -i'); if (found) return found;
  }
  return null;
}

const GIT_GLOBAL = { shortArg: 'Cc', shortFlag: 'pP', longArg: ['git-dir', 'work-tree', 'namespace', 'super-prefix', 'config-env', 'list-cmds', 'attr-source'], longFlag: ['exec-path', 'no-pager', 'paginate', 'bare', 'no-replace-objects', 'literal-pathspecs', 'glob-pathspecs', 'noglob-pathspecs', 'icase-pathspecs', 'no-optional-locks', 'no-advice', 'html-path', 'man-path', 'info-path', 'version', 'help'], stopAtOperand: true, strict: true };
const GIT_CLONE = { shortArg: 'boucj', shortFlag: 'nlsqv', longArg: ['branch', 'origin', 'config', 'depth', 'reference', 'reference-if-able', 'separate-git-dir', 'template', 'jobs', 'upload-pack', 'shallow-since', 'shallow-exclude', 'filter', 'server-option', 'bundle-uri', 'revision', 'also-filter-submodules'], longFlag: ['bare', 'mirror', 'no-checkout', 'local', 'no-hardlinks', 'shared', 'single-branch', 'tags', 'recursive', 'recurse-submodules', 'shallow-submodules', 'quiet', 'verbose', 'progress', 'dissociate', 'sparse', 'remote-submodules', 'reject-shallow', 'ipv4', 'ipv6', 'force', 'hardlinks', 'checkout'], allowNo: true, strict: true };
const GIT_WORKTREE_ADD = { shortArg: 'bB', shortFlag: 'fqd', longArg: ['reason'], longFlag: ['force', 'detach', 'checkout', 'lock', 'quiet', 'track', 'guess-remote', 'orphan', 'relative-paths', 'sparse'], allowNo: true, strict: true };

const GIT_WORKTREE_OTHER = { shortArg: '', shortFlag: 'fnv', longArg: ['reason', 'expire'], longFlag: ['force', 'dry-run', 'verbose'], allowNo: false, strict: true };
const GIT_INIT = { shortArg: 'b', shortFlag: 'q', longArg: ['template', 'separate-git-dir', 'initial-branch', 'object-format', 'ref-format'], longFlag: ['bare', 'quiet', 'shared'], strict: true };
const GIT_ARCHIVE = { shortArg: 'o', shortFlag: '0123456789lv', longArg: ['output', 'format', 'prefix', 'add-file', 'add-virtual-file', 'remote'], longFlag: ['list', 'verbose', 'worktree-attributes'], strict: true };
const GIT_READ_ONLY = new Set(['status', 'log', 'diff', 'show', 'rev-parse', 'ls-files', 'ls-tree', 'cat-file', 'rev-list', 'describe', 'blame', 'shortlog', 'grep', 'show-ref', 'for-each-ref', 'ls-remote', 'merge-base', 'diff-tree', 'diff-index', 'diff-files', 'name-rev', 'whatchanged', 'count-objects', 'check-ignore', 'check-attr', 'var', 'version', 'help', 'reflog', 'range-diff', 'cherry', 'verify-commit', 'verify-tag']);
const GIT_DIFF_FAMILY = new Set(['log', 'diff', 'show', 'diff-tree', 'diff-index', 'diff-files', 'whatchanged', 'range-diff']);
const GIT_LOCAL_WRITERS = new Set(['add', 'commit', 'checkout', 'switch', 'restore', 'reset', 'merge', 'rebase', 'stash', 'fetch', 'pull', 'push', 'gc', 'cherry-pick', 'revert', 'mv', 'rm', 'clean']);
const GIT_CODE_OPTIONS = /^--(upload-pack|receive-pack|exec|exec-path|open-files-in-pager)(=|$)/;
// GIT_* variables that cannot move the repository, change configuration or run a program.
const GIT_HARMLESS_ENV = [/^GIT_TERMINAL_PROMPT$/, /^GIT_PAGER$/, /^GIT_(AUTHOR|COMMITTER)_(NAME|EMAIL|DATE)$/];
const GIT_BUDGET_MS = 5000;

/** The first GIT_* variable, in the hook's own process env or the command's tracked/prefix env, that is not harmless. */
function gitEnvViolation(scope, ctx) {
  const bad = (name, value) => name.startsWith('GIT_') && !GIT_HARMLESS_ENV.some((pattern) => pattern.test(name)) && !(name === 'GIT_EDITOR' && (value === 'true' || value === ':')) && !(name === 'GIT_OPTIONAL_LOCKS' && value === '0');
  for (const [name, value] of scope.vars) if (bad(name, typeof value === 'string' ? value : undefined)) return name;
  for (const [name, value] of Object.entries(ctx.env)) if (bad(name, value)) return name;
  return null;
}

/** Default git runner: bounded, read-only, with a scrubbed environment: PATH, the EFFECTIVE HOME and XDG_CONFIG_HOME (they select the global configuration), no GIT_*. */
export function gitChildEnv({ home, xdgConfigHome, path: searchPath }) {
  const env = { PATH: searchPath ?? '', LC_ALL: 'C', GIT_OPTIONAL_LOCKS: '0' };
  if (home !== undefined) env.HOME = home; // undefined = the real child has no HOME (env -i): never invent one
  if (xdgConfigHome !== undefined) env.XDG_CONFIG_HOME = xdgConfigHome;
  return env;
}

export function defaultGitRunner(args, { cwd, timeout, home, xdgConfigHome, path: searchPath }) {
  const env = gitChildEnv({ home, xdgConfigHome, path: searchPath });
  const result = spawnSync('git', args, { cwd, timeout, encoding: 'utf8', maxBuffer: 1_048_576, windowsHide: true, env });
  if (result.error) throw result.error;
  return { status: result.status, stdout: result.stdout };
}

/** Run git through the ctx seam. All spawns in one hook call share a 5 s budget; no time left means no spawn and a refusal. */
/**
 * The HOME and XDG_CONFIG_HOME git would see for THIS command (they pick the global configuration the probe must read): the
 * value the command sets (prefix assignment, export, env), else the hook's inherited one. A value set by the command that
 * cannot be resolved literally refuses; they are never silently dropped.
 */
function gitProbeEnv(scope, ctx) {
  const probe = { home: ctx.env.HOME ?? ctx.home, xdgConfigHome: ctx.env.XDG_CONFIG_HOME };
  for (const [name, key] of [['HOME', 'home'], ['XDG_CONFIG_HOME', 'xdgConfigHome']]) {
    // A plain `NAME=v` statement (no export, and NAME is not already in the environment) changes only the shell, not git's environment.
    if (scope.vars.has(UNEXPORTED + name)) return { error: `${name} was set by a plain shell assignment that is not exported, so the guard cannot tell which value git sees (use "export ${name}=..." or a prefix assignment)` };
    if (scope.vars.has(UNKNOWN_EXPORT + name)) return { error: `whether ${name} is exported is not known here (a conditional export, export -n, an unmodelled declare/export flag or a dynamic name), and it selects the global git configuration the probe must read` };
    if (!scope.vars.has(name)) {
      if (scope.unknownAll) return { error: `an earlier eval/source/read may have set ${name}, and it selects the global git configuration the probe must read` };
      continue;
    }
    const value = scope.vars.get(name);
    if (value === UNSET) { probe[key] = undefined; continue; } // env -i: the child has none
    if (typeof value !== 'string') return { error: `${name} is set by this command to a value that cannot be resolved literally, and it selects the global git configuration the probe must read` };
    probe[key] = value;
  }
  return probe;
}

function gitRun(ctx, args, cwd, probe) {
  const now = ctx.clock();
  if (ctx.gitDeadline === null) ctx.gitDeadline = now + GIT_BUDGET_MS;
  const left = ctx.gitDeadline - now;
  if (left <= 0) return { ok: false, why: 'the 5 s budget for inspecting git is used up, so git was not run' };
  try {
    const result = ctx.gitRunner(args, { cwd, timeout: Math.min(left, GIT_BUDGET_MS), home: probe.home, xdgConfigHome: probe.xdgConfigHome, path: ctx.env.PATH });
    if (!result || result.status !== 0) return { ok: false, status: result?.status ?? null, why: `git ${args.slice(-3).join(' ')} failed or timed out (status ${result?.status ?? 'none'})` };
    return { ok: true, out: String(result.stdout ?? '') };
  } catch (error) { return { ok: false, status: null, why: `git could not be run (${error?.code ?? error?.message})` }; }
}

/**
 * Repo-local writers are admitted only on git's OWN resolution of the repository: no GIT_* overrides (outside a harmless
 * list), then `git -c core.fsmonitor=false -c core.untrackedCache=false rev-parse --path-format=absolute --show-toplevel
 * --absolute-git-dir --git-common-dir` run read-only in the repository directory (honouring -C) resolves global and
 * included configuration, gitdir: files, commondir and core.worktree. Each of the three outputs, resolved physically, and the
 * repository directory itself must be on an agent volume. A failure, timeout, no time left or unparseable output refuses.
 * Hooks, filters and a global core.hooksPath are code execution outside the guard (README limits).
 */
function gitRepositoryClosure(sub, scope, cwd, ctx) {
  const refuseRepo = (why) => refuse(`git ${sub}: ${why}`, 'Run git inside a normal repository whose work tree, git directory and common directory (as git itself resolves them) are all on /Volumes/D, with no GIT_* overrides.');
  const offAgent = (what, why) => violation('non-agent-volume', `git ${sub} writes into ${what}, ${why}`, requiredFor.path);
  const env = gitEnvViolation(scope, ctx);
  if (env) return refuseRepo(`${env} is set, which can move or reconfigure the repository away from where the guard checks it`);
  if (scope.unknownAll) return refuseRepo('an earlier eval/source/read may have set GIT_DIR or another repository override');
  if (!cwd) return refuseRepo('the repository directory is not known (an earlier cd may not have run)');
  const repo = agentDestination({ v: cwd, dyn: false }, null, ctx);
  if (!repo.ok) return repo.unknown ? refuseRepo(repo.why) : offAgent(`the repository at ${cwd}`, `which is not on an agent volume: ${repo.why}`);
  const probe = gitProbeEnv(scope, ctx);
  if (probe.error) return refuseRepo(probe.error);
  const run = gitRun(ctx, ['-c', 'core.fsmonitor=false', '-c', 'core.untrackedCache=false', 'rev-parse', '--path-format=absolute', '--show-toplevel', '--absolute-git-dir', '--git-common-dir'], repo.real, probe);
  if (!run.ok) return refuseRepo(run.why);
  const lines = run.out.split(/\r?\n/).filter(Boolean);
  if (lines.length !== 3 || lines.some((line) => !line.startsWith('/'))) return refuseRepo('git rev-parse printed something other than three absolute paths');
  const labels = ['the work tree', 'the git directory', 'the common git directory'];
  for (let index = 0; index < 3; index += 1) {
    const real = physicalDir(lines[index], ctx);
    if (!real) return refuseRepo(`${labels[index]} ${lines[index]} cannot be resolved`);
    const checked = agentDestination({ v: real, dyn: false }, null, ctx);
    if (!checked.ok) return checked.unknown ? refuseRepo(checked.why) : offAgent(`${labels[index]} ${real}`, `which is not on an agent volume: ${checked.why}`);
  }
  return null;
}

/** The worktrees git lists for the repository at `cwd` (physical): { roots: [{ raw, real }] } or { error }. */
function gitWorktreeRoots(ctx, probe, cwd) {
  const run = gitRun(ctx, ['worktree', 'list', '--porcelain'], cwd, probe);
  if (!run.ok) return { error: run.why };
  const roots = [];
  for (const line of run.out.split(/\r?\n/)) {
    if (!line.startsWith('worktree ')) continue;
    const raw = line.slice(9);
    const real = raw.startsWith('/') ? physicalDir(raw, ctx) : null;
    if (!real) return { error: `the worktree root ${raw} cannot be resolved` };
    roots.push({ raw, real });
  }
  return roots.length ? { roots } : { error: 'git worktree list printed no worktree' };
}

/** Classify a git remote URL or path: network (allowed), local (a path to check), or refused (a remote helper). */
function gitRemoteKind(url) {
  if (/^[A-Za-z][A-Za-z0-9+.-]*::/.test(url)) return { refuse: `the remote helper URL ${url} runs a program` };
  const scheme = /^([A-Za-z][A-Za-z0-9+.-]*):\/\/(.*)$/.exec(url);
  if (scheme) {
    if (scheme[1].toLowerCase() !== 'file') return { network: true };
    const rest = scheme[2]; const slash = rest.indexOf('/');
    const host = slash < 0 ? rest : rest.slice(0, slash);
    if (host && host !== 'localhost') return { network: true };
    try { return { local: decodeURIComponent(slash < 0 ? '/' : rest.slice(slash)) }; } catch { return { refuse: `the file:// URL ${url} cannot be decoded` }; }
  }
  if (/^[^/]*:/.test(url)) return { network: true }; // scp-like host:path
  return { local: url };
}

/** push/fetch/pull: a local-path or file:// remote must resolve physically onto an agent volume; network URLs are fine. */
function gitRemoteCheck(sub, after, cwd, ctx, probe) {
  const refuseRemote = (why) => refuse(`git ${sub}: ${why}`, 'Use a network remote, or a local remote on /Volumes/D given as an absolute path or a remote name whose URL is such a path.');
  const valued = sub === 'push' ? ['-o', '--push-option', '--repo'] : sub === 'fetch' ? ['-j', '--jobs', '--depth', '--deepen', '--shallow-since', '--shallow-exclude', '--refmap', '--server-option', '-o', '--negotiation-tip', '--filter', '--repo']
    : ['-j', '--jobs', '--depth', '--deepen', '--shallow-since', '--shallow-exclude', '--refmap', '--server-option', '-o', '--negotiation-tip', '--filter', '-s', '--strategy', '-X', '--strategy-option', '--cleanup'];
  let repository = null;
  for (let index = 0; index < after.length; index += 1) {
    const word = after[index];
    if (word.v === '--') { repository = after[index + 1] ?? null; break; }
    if (!word.dyn && /^--repo=/.test(word.v)) { repository = sliceWord(word, 7); break; }
    if (!word.dyn && word.v === '--repo') { repository = after[index + 1] ?? null; break; }
    if (!word.dyn && word.v.startsWith('-') && word.v.length > 1) { if (valued.includes(word.v)) index += 1; continue; }
    repository = word; break; // the first operand is the repository (dynamic ones included: they are refused below)
  }
  if (repository?.dyn) return refuseRemote('the remote operand is built from a shell expansion, so whether it is a local path cannot be resolved');
  const urls = [];
  const fromRemotes = (name) => {
    const run = gitRun(ctx, ['remote', '-v'], cwd, probe);
    if (!run.ok) return { error: run.why };
    const kind = sub === 'push' ? 'push' : 'fetch';
    const found = []; let known = false;
    for (const line of run.out.split(/\r?\n/)) {
      const match = /^(\S+)\t(.+) \((fetch|push)\)$/.exec(line);
      if (!match) continue;
      if (name === null || match[1] === name) { known = true; if (match[3] === kind) found.push(match[2]); }
    }
    return { found, known };
  };
  if (repository && /[/:]|^[.~]/.test(repository.v)) urls.push(repository.v);
  else {
    const remotes = fromRemotes(repository ? repository.v : null);
    if (remotes.error) return refuseRemote(remotes.error);
    if (repository && !remotes.known) urls.push(repository.v); // not a remote name: git treats it as a relative path
    else urls.push(...remotes.found);
  }
  for (const url of urls) {
    const kind = gitRemoteKind(url);
    if (kind.refuse) return refuseRemote(kind.refuse);
    if (kind.network) continue;
    const absolute = absolutePath(kind.local, cwd);
    if (!absolute) return refuseRemote(`the local remote "${kind.local}" is relative and the repository directory is not known`);
    const real = physicalDir(absolute, ctx);
    if (!real) return refuseRemote(`the local remote ${kind.local} cannot be resolved`);
    const checked = agentDestination({ v: real, dyn: false }, null, ctx);
    if (!checked.ok) return checked.unknown ? refuseRemote(checked.why) : violation('non-agent-volume', `git ${sub} writes to the local repository ${real}, which is not on an agent volume: ${checked.why}`, requiredFor.path);
  }
  return null;
}

/** Read-only-looking git forms that refresh the index (.git/index, index.lock) unless optional locks are off. */
const GIT_INDEX_TOUCHING = new Set(['status', 'diff', 'diff-files', 'diff-index', 'describe']);
function gitTouchesIndex(name, after) {
  if (name === 'diff') return !after.some((word) => word.v === '--no-index'); // --no-index never reads a repository
  if (GIT_INDEX_TOUCHING.has(name)) return true;
  if (name !== 'ls-files') return false;
  return after.some((word) => {
    if (word.dyn && !word.prefix) return true; // could be -m
    if (word.v === '--') return false;
    if (word.v.startsWith('--')) { const long = word.v.slice(2).split('=')[0]; return long.length > 0 && 'modified'.startsWith(long) || long.length > 0 && 'deleted'.startsWith(long); }
    return /^-[A-Za-z]*[md]/.test(word.v);
  });
}

/** Options of any git command that WRITE a file: --output (and its unique abbreviations --out, --outp, --outpu). */
function gitOutputFiles(args, strictDynamic) {
  const files = [];
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index];
    if (word.v === '--') break;
    if (word.dyn && !word.prefix) { if (strictDynamic) return { error: 'an argument starts with an unevaluated expansion and could be --output=FILE' }; continue; }
    if (/^--out(?:p(?:u(?:t)?)?)?(?:=|$)/.test(word.v)) {
      const eq = word.v.indexOf('=');
      const value = eq >= 0 ? sliceWord(word, eq + 1) : args[index + 1];
      if (!value) return { error: `${word.v} has no value` };
      if (eq < 0) index += 1;
      files.push(value);
    }
  }
  return { files };
}

/** Is `git tag|branch ARGS` a listing (read-only) rather than a create/delete/rename (repo-local write)? */
function gitListing(sub, args) {
  const letters = sub === 'branch' ? 'dDmMcCfut' : 'dasumFfe';
  const longWrite = sub === 'branch'
    ? ['delete', 'move', 'copy', 'force', 'set-upstream-to', 'unset-upstream', 'edit-description', 'track', 'no-track', 'create-reflog', 'recurse-submodules']
    : ['delete', 'annotate', 'sign', 'local-user', 'message', 'file', 'force', 'edit', 'create-reflog'];
  const valued = ['contains', 'no-contains', 'merged', 'no-merged', 'points-at', 'sort', 'format', 'abbrev', 'column'];
  let list = false; const operands = [];
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index];
    if (word.dyn && !word.prefix) return null; // could be a write flag
    if (word.v === '--') { operands.push(...args.slice(index + 1)); break; }
    if (word.v.startsWith('--')) {
      const name = word.v.slice(2).split('=')[0];
      if (name === 'list') { list = true; continue; }
      if (longWrite.some((candidate) => name && candidate.startsWith(name))) return false;
      if (valued.some((candidate) => name && candidate.startsWith(name)) && !word.v.includes('=')) index += 1;
      continue;
    }
    if (word.v.startsWith('-') && word.v.length > 1) {
      if ([...word.v.slice(1)].some((letter) => letters.includes(letter))) return false;
      if (word.v.includes('l')) list = true;
      continue;
    }
    operands.push(word);
  }
  return list || operands.length === 0;
}

/** git 2.47 `git config` long options (including the subcommand-mode ones). Negated forms git accepts are listed explicitly. */
const GIT_CONFIG_LONG = ['global', 'system', 'local', 'worktree', 'file', 'blob', 'get', 'get-all', 'get-regexp', 'get-urlmatch', 'replace-all', 'add', 'unset', 'unset-all', 'rename-section', 'remove-section', 'list',
  'fixed-value', 'type', 'no-type', 'bool', 'int', 'bool-or-int', 'path', 'expiry-date', 'null', 'name-only', 'includes', 'no-includes', 'show-origin', 'show-scope', 'get-colorbool', 'get-color', 'edit',
  'default', 'comment', 'all', 'regexp', 'value', 'url', 'show-names', 'no-show-names', 'append'];
/** Long options that take a value ("--comment X" or "--comment=X"). */
const GIT_CONFIG_VALUE_OPTIONS = new Set(['file', 'blob', 'type', 'default', 'comment', 'value', 'url']);

/**
 * Parse `git config` arguments ONCE into { options: [{ name, value }], operands: [] }. Long options are canonicalised
 * (exact name wins, a unique prefix maps to the full name; unknown or ambiguous refuses). A value-taking option consumes its
 * value word (the next word, or the part after "=", or for -f/-t the rest of the cluster or the next word) so that value is
 * never classified as an option or an operand. `--` ends the options. Short options: -l = list, -e = edit, -z = null,
 * -f = file, -t = type. Every read/write/scope/edit/file decision is made from these nodes only.
 * Dynamic option-like words never reach here (the caller refuses them).
 */
function parseGitConfigArgs(words) {
  const options = []; const operands = [];
  for (let index = 0; index < words.length; index += 1) {
    const word = words[index];
    if (word.v === '--') { operands.push(...words.slice(index + 1)); break; }
    if (word.v.startsWith('--')) {
      const eq = word.v.indexOf('=');
      const typed = word.v.slice(2, eq < 0 ? undefined : eq);
      const candidates = GIT_CONFIG_LONG.includes(typed) ? [typed] : GIT_CONFIG_LONG.filter((option) => typed !== '' && option.startsWith(typed));
      if (candidates.length === 0) return { error: `--${typed} is not a git config option the guard knows` };
      if (candidates.length > 1) return { error: `--${typed} is ambiguous (${candidates.map((option) => `--${option}`).join(', ')})` };
      const name = candidates[0];
      if (!GIT_CONFIG_VALUE_OPTIONS.has(name)) { options.push({ name, value: null }); continue; }
      const value = eq >= 0 ? sliceWord(word, eq + 1) : words[index + 1];
      if (!value) return { error: `--${name} has no value` };
      if (eq < 0) index += 1;
      options.push({ name, value });
    } else if (word.v.startsWith('-') && word.v.length > 1) {
      for (let at = 1; at < word.v.length; at += 1) {
        const letter = word.v[at];
        if (letter === 'f' || letter === 't') {
          const value = at + 1 < word.v.length ? sliceWord(word, at + 1) : words[index + 1];
          if (!value) return { error: `-${letter} has no value` };
          if (at + 1 >= word.v.length) index += 1;
          options.push({ name: letter === 'f' ? 'file' : 'type', value });
          break; // the rest of the cluster was the value
        }
        const named = { l: 'list', e: 'edit', z: 'null' }[letter];
        if (named) options.push({ name: named, value: null });
      }
    } else operands.push(word);
  }
  return { options, operands };
}

/**
 * git. Global options are parsed strictly; `-C` applies in order (each relative one against the previous, resolved
 * physically); `-c`, `--config-env`, `--git-dir`, `--work-tree`, `--namespace` and `--exec-path=` are REFUSED. Subcommands:
 *   (a) read-only forms are allowed;
 *   (b) repo-local writers (add, commit, checkout, switch, restore, reset, merge, rebase, stash, fetch, pull, push, gc,
 *       tag/branch create, remote add/set-url, config set ...) are admitted only on the closure gitRepositoryClosure checks;
 *       push/fetch/pull also resolve their remote (gitRemoteCheck);
 *   (c) output writers are destinations on every subcommand: --output FILE, init [DIR], clone, worktree add PATH / move,
 *       archive -o, bundle create FILE, format-patch -o DIR (or the cwd), config --file FILE;
 *   (d) any other subcommand or unparsed form is refused.
 * clone: the destination is the explicit DIR (second operand) or <cwd>/<repository name>. worktree add: PATH, the FIRST
 * operand (never the trailing commit-ish). Unknown options, extra operands and unresolved words are refused.
 */
function gitRule(rest, scope, ctx) {
  const refuseGit = (why) => refuse(`git: ${why}`, 'Run git with literal options and an absolute destination, for example "git clone URL /Volumes/D/scratch/x".');
  const globals = parseOptions(rest, GIT_GLOBAL);
  if (globals.error) return refuseGit(`${globals.error}; the guard does not know that global option's arity`);
  const globalWords = rest.slice(0, rest.length - globals.operands.length);
  if (globals.opts.some((opt) => ['c', 'config-env', 'git-dir', 'work-tree', 'namespace'].includes(opt.name)) || globalWords.some((word) => /^--exec-path=/.test(word.v))) {
    return refuseGit('-c, --config-env, --git-dir, --work-tree, --namespace and --exec-path= can reconfigure git, move the repository or run programs, so they are refused');
  }
  let cwd = scope.cwd;
  for (const opt of globals.opts.filter((entry) => entry.name === 'C')) {
    if (opt.value.dyn) return refuseGit('-C is built from a shell expansion');
    const target = cwd === null && !opt.value.v.startsWith('/') ? null : absolutePath(opt.value.v, cwd);
    cwd = target ? physicalDir(target, ctx) : null; // chdir(2): symlinks before ".."
  }
  const inner = { ...scope, cwd };
  const [sub, ...after] = globals.operands;
  // Every git invocation, read-only forms included: a GIT_* override can move the repository or run a program.
  const envBad = gitEnvViolation(scope, ctx);
  if (envBad) return refuseGit(`${envBad} is set, which can move or reconfigure git (only GIT_TERMINAL_PROMPT, GIT_PAGER, GIT_EDITOR=true|:, GIT_OPTIONAL_LOCKS=0 and GIT_AUTHOR_*/GIT_COMMITTER_* name/email/date are allowed)`);
  if (!sub) return null; // git --version, git --help
  if (sub.dyn) return refuseGit('the subcommand is built from a shell expansion');
  const name = sub.v;
  const destinations = [];
  const lockValue = scope.vars.has('GIT_OPTIONAL_LOCKS') ? scope.vars.get('GIT_OPTIONAL_LOCKS') : ctx.env.GIT_OPTIONAL_LOCKS;
  const optionalLocksOff = globals.opts.some((opt) => opt.name === 'no-optional-locks') || lockValue === '0';
  // (c) every subcommand: --output FILE. Dynamic leading words are refused only where --output is a diff option.
  const outputs = gitOutputFiles(after, GIT_DIFF_FAMILY.has(name) || name === 'format-patch');
  if (outputs.error) return refuseGit(`${name}: ${outputs.error}`);
  for (const word of outputs.files) destinations.push({ word, via: `git ${name} --output` });
  const finish = () => {
    for (const destination of destinations) {
      const found = protectedWrite({ ...destination.word, prefix: destination.word.prefix ?? destination.word.v }, inner, ctx, destination.via);
      if (found) return found;
    }
    return null;
  };
  const writer = () => { // (b) a repo-local writer
    if (after.some((word) => GIT_CODE_OPTIONS.test(word.v) || (name === 'rebase' && (word.v === '-x' || /^--exec/.test(word.v))))) return refuseGit(`${name}: an option runs an external command (--upload-pack, --receive-pack, --exec)`);
    const closed = gitRepositoryClosure(name, scope, cwd, ctx);
    if (closed) return closed;
    if (['push', 'fetch', 'pull'].includes(name)) { const remote = gitRemoteCheck(name, after, cwd, ctx, gitProbeEnv(scope, ctx)); if (remote) return remote; }
    return finish();
  };
  if (name === 'clone') {
    const parsed = parseOptions(after, GIT_CLONE);
    if (parsed.error) return refuseGit(`clone: ${parsed.error}`);
    if (parsed.opts.some((opt) => ['u', 'upload-pack'].includes(opt.name))) return refuseGit('clone: --upload-pack runs an external command');
    for (const opt of parsed.opts) if (opt.name === 'separate-git-dir' && opt.value) destinations.push({ word: opt.value, via: 'git clone --separate-git-dir' });
    const operands = parsed.operands;
    if (operands.length === 0 || operands.length > 2) return refuseGit(`clone takes a repository and an optional directory, got ${operands.length} operands`);
    if (operands.length === 2) destinations.push({ word: operands[1], via: 'git clone' });
    else {
      const url = operands[0];
      if (url.dyn) return refuseGit('clone: the repository operand is built from a shell expansion, so the default directory name cannot be resolved');
      if (!cwd) return refuseGit('clone without a directory creates it in the working directory, which is not known');
      const dirName = url.v.replace(/[/\\]+$/, '').split(/[/\\:]/).at(-1).replace(/\.git$/, '');
      if (!dirName || dirName === '.' || dirName === '..') return refuseGit(`clone cannot derive a directory name from "${url.v}"`);
      const bare = parsed.opts.some((opt) => ['bare', 'mirror'].includes(opt.name));
      destinations.push({ word: { v: `${cwd.replace(/\/+$/, '')}/${dirName}${bare ? '.git' : ''}`, dyn: false }, via: 'git clone (default directory)' });
    }
    return finish();
  }
  if (name === 'init') {
    const parsed = parseOptions(after, GIT_INIT);
    if (parsed.error) return refuseGit(`init: ${parsed.error}`);
    for (const opt of parsed.opts) if (opt.name === 'separate-git-dir' && opt.value) destinations.push({ word: opt.value, via: 'git init --separate-git-dir' });
    if (parsed.operands.length > 1) return refuseGit(`init takes one optional directory, got ${parsed.operands.length} operands`);
    if (parsed.operands.length === 1) destinations.push({ word: parsed.operands[0], via: 'git init' });
    else if (!cwd) return refuseGit('init without a directory writes .git into the working directory, which is not known');
    else destinations.push({ word: { v: cwd, dyn: false }, via: 'git init (the working directory)' });
    const found = finish();
    if (found) return found;
    // Reinit and nested init: when the target exists, git's own resolution there must be on the agent volume. A target that
    // already has a .git entry (dir or gitdir: file) or sits inside a repository is reinitialised through its existing
    // gitdir/commondir, so a redirected one (E, internal) is denied. Only a target with no .git entry on which git says
    // "not a repository" (status 128) is a fresh init that checks the target alone.
    const targetWord = parsed.operands[0];
    const target = targetWord ? (targetWord.v.startsWith('/') ? targetWord.v : cwd ? absolutePath(targetWord.v, cwd) : null) : cwd;
    if (!target) return refuseGit('init: the target directory is not known');
    const real = physicalDir(target, ctx);
    if (!real) return refuseGit(`init: the target ${target} cannot be resolved`);
    let present; let dotGit;
    try { present = ctx.fs.kind(real); dotGit = present ? ctx.fs.kind(`${real.replace(/\/+$/, '')}/.git`) : null; } catch (error) { return refuseGit(`init: the target cannot be inspected (${error?.code ?? error?.message})`); }
    if (!present) return null; // a new directory: nothing exists to reinitialise
    const probe = gitProbeEnv(scope, ctx);
    if (probe.error) return refuseGit(`init: ${probe.error}`);
    const unresolved = refuse('git init: the existing repository at the target cannot be resolved (a redirected or broken reinit is refused)', 'Run git init in a new directory, or in a repository whose work tree, git directory and common directory are all on /Volumes/D.');
    const run = gitRun(ctx, ['-c', 'core.fsmonitor=false', '-c', 'core.untrackedCache=false', 'rev-parse', '--path-format=absolute', '--show-toplevel', '--absolute-git-dir', '--git-common-dir'], real, probe);
    if (!run.ok) return run.status === 128 && dotGit === null ? null : unresolved;
    const lines = run.out.split(/\r?\n/).filter(Boolean);
    if (lines.length !== 3 || lines.some((line) => !line.startsWith('/'))) return unresolved;
    for (const line of lines) {
      const resolved = physicalDir(line, ctx);
      if (!resolved) return unresolved;
      const checked = agentDestination({ v: resolved, dyn: false }, null, ctx);
      if (!checked.ok) return checked.unknown ? unresolved : violation('non-agent-volume', `git init reinitialises into ${resolved}, which is not on an agent volume: ${checked.why}`, requiredFor.path);
    }
    return null;
  }
  if (name === 'archive') {
    const parsed = parseOptions(after, GIT_ARCHIVE);
    if (parsed.error) return refuseGit(`archive: ${parsed.error}`);
    if (parsed.opts.some((opt) => ['remote'].includes(opt.name))) return refuseGit('archive --remote contacts another repository');
    for (const opt of parsed.opts) if (['o', 'output'].includes(opt.name) && opt.value) destinations.push({ word: opt.value, via: 'git archive --output' });
    return finish();
  }
  if (name === 'bundle') {
    const action = after[0];
    if (!action || action.dyn) return refuseGit('bundle: the action is missing or built from a shell expansion');
    if (['verify', 'list-heads'].includes(action.v)) return finish();
    if (action.v === 'unbundle') return writer();
    if (action.v !== 'create') return refuseGit(`bundle ${action.v} is not understood`);
    let file = null;
    for (const word of after.slice(1)) {
      if (word.dyn && !word.prefix) return refuseGit('bundle create: an argument starts with an unevaluated expansion');
      if (word.v.startsWith('-')) continue; // --quiet, --progress, --version=N
      file = word; break;
    }
    if (!file) return refuseGit('bundle create has no file operand');
    destinations.push({ word: file, via: 'git bundle create' });
    return finish();
  }
  if (name === 'format-patch') {
    let directory = null; let stdout = false;
    for (let index = 0; index < after.length; index += 1) {
      const word = after[index];
      if (word.v === '--') break;
      if (word.dyn && !word.prefix) return refuseGit('format-patch: an argument starts with an unevaluated expansion and could be -o');
      if (word.v === '-o') { directory = after[index + 1]; index += 1; if (!directory) return refuseGit('format-patch -o has no directory'); }
      else if (/^-o.+/.test(word.v) && !word.v.startsWith('--')) directory = sliceWord(word, 2);
      else if (/^--output-d(?:i(?:r(?:e(?:c(?:t(?:o(?:r(?:y)?)?)?)?)?)?)?)?(?:=|$)/.test(word.v)) {
        const eq = word.v.indexOf('=');
        directory = eq >= 0 ? sliceWord(word, eq + 1) : after[index + 1];
        if (eq < 0) index += 1;
        if (!directory) return refuseGit('format-patch --output-directory has no directory');
      } else if (/^--std(?:o(?:u(?:t)?)?)?$/.test(word.v)) stdout = true;
    }
    if (directory) destinations.push({ word: directory, via: 'git format-patch --output-directory' });
    else if (!stdout) {
      if (!cwd) return refuseGit('format-patch writes patch files into the working directory, which is not known');
      destinations.push({ word: { v: cwd, dyn: false }, via: 'git format-patch (writes into the working directory)' });
    }
    return finish();
  }
  if (name === 'worktree') {
    const action = after[0];
    if (!action) return finish();
    if (action.dyn) return refuseGit('the worktree subcommand is built from a shell expansion');
    if (action.v === 'list') return finish();
    if (!['add', 'move', 'remove', 'prune', 'lock', 'unlock', 'repair'].includes(action.v)) return refuseGit(`worktree ${action.v} is not understood`);
    // The SOURCE repository (the -C repo) holds .git/worktrees/<name>: its work tree, git dir and common dir must be on D.
    const closed = gitRepositoryClosure(`worktree ${action.v}`, scope, cwd, ctx);
    if (closed) return closed;
    const parsed = parseOptions(after.slice(1), action.v === 'add' ? GIT_WORKTREE_ADD : GIT_WORKTREE_OTHER);
    if (parsed.error) return refuseGit(`worktree ${action.v}: ${parsed.error}`);
    const operands = parsed.operands;
    const probe = gitProbeEnv(scope, ctx);
    if (probe.error) return refuseGit(`worktree ${action.v}: ${probe.error}`);
    const offRoot = (root, why) => violation('non-agent-volume', `git worktree ${action.v} affects the worktree ${root.real}, which is not on an agent volume: ${why}`, requiredFor.path);
    // Resolve an existing worktree operand to the root git lists (exact physical path, else git's unique-suffix match).
    const existing = (word, listed) => {
      if (word.dyn) return { error: 'a worktree operand is built from a shell expansion' };
      const absolute = absolutePath(word.v, cwd);
      const real = absolute ? physicalDir(absolute, ctx) : null;
      const exact = real ? listed.filter((root) => root.real.toLowerCase() === real.toLowerCase()) : [];
      if (exact.length === 1) return { root: exact[0] };
      const suffix = listed.filter((root) => root.raw.endsWith(`/${word.v.replace(/^\/+/, '')}`));
      if (suffix.length === 1 && !word.v.startsWith('/')) return { root: suffix[0] };
      return { error: `${word.v} is not a worktree git lists for this repository` };
    };
    const needRoot = (word, listed) => {
      const found = existing(word, listed);
      if (found.error) return refuseGit(`worktree ${action.v}: ${found.error}`);
      const checked = agentDestination({ v: found.root.real, dyn: false }, null, ctx);
      return checked.ok ? null : checked.unknown ? refuseGit(`worktree ${action.v}: ${checked.why}`) : offRoot(found.root, checked.why);
    };
    const listAll = () => gitWorktreeRoots(ctx, probe, cwd);
    if (action.v === 'add') {
      if (operands.length < 1 || operands.length > 2) return refuseGit(`worktree add takes PATH and an optional commit-ish, got ${operands.length} operands`);
      destinations.push({ word: operands[0], via: 'git worktree add' }); // PATH, never the trailing commit-ish
      return finish();
    }
    if (action.v === 'move') {
      if (operands.length !== 2) return refuseGit(`worktree move takes a worktree and a new location, got ${operands.length} operands`);
      destinations.push({ word: operands[1], via: 'git worktree move' });
      const found = finish();
      if (found) return found;
      const listed = listAll();
      if (listed.error) return refuseGit(`worktree move: ${listed.error}`);
      return needRoot(operands[0], listed.roots); // the OLD path is affected by the move too
    }
    if (action.v === 'remove' || action.v === 'lock' || action.v === 'unlock') {
      if (operands.length !== 1) return refuseGit(`worktree ${action.v} takes exactly one worktree, got ${operands.length} operands`);
      const listed = listAll();
      if (listed.error) return refuseGit(`worktree ${action.v}: ${listed.error}`);
      return needRoot(operands[0], listed.roots);
    }
    if (action.v === 'repair') {
      const listed = listAll();
      if (listed.error) return refuseGit(`worktree repair: ${listed.error}`);
      for (const word of operands) destinations.push({ word, via: 'git worktree repair' });
      const found = finish();
      if (found) return found;
      for (const root of listed.roots) { const checked = agentDestination({ v: root.real, dyn: false }, null, ctx); if (!checked.ok) return checked.unknown ? refuseGit(`worktree repair: ${checked.why}`) : offRoot(root, checked.why); }
      return null;
    }
    if (operands.length) return refuseGit('worktree prune takes no operands');
    return finish(); // prune: only the source repository's own .git/worktrees (closure checked above)
  }
  if (name === 'tag' || name === 'branch') {
    const listing = gitListing(name, after);
    if (listing === null) return refuseGit(`${name}: an argument starts with an unevaluated expansion and could be a write flag`);
    return listing ? finish() : writer();
  }
  if (name === 'remote') {
    const action = after.find((word) => !(word.v.startsWith('-') && word.v.length > 1));
    if (!action || (!action.dyn && ['show', 'get-url'].includes(action.v))) return finish();
    if (action.dyn) return refuseGit('the remote action is built from a shell expansion');
    return writer();
  }
  if (name === 'config') {
    if (after.some((word) => word.dyn && (!word.prefix || word.v.startsWith('-')))) return refuseGit('config: an option or argument is built from an unevaluated expansion and could be -f/--file or --global; write the options, key and value as literals');
    // git's parse-options accepts unique PREFIXES of long options (--glo = --global). Parse once into option nodes (values
    // consumed by their option) and decide only from those nodes; unknown or ambiguous long options refuse.
    const parsed = parseGitConfigArgs(after);
    if (parsed.error) return refuseGit(`config: ${parsed.error}`);
    const { options, operands } = parsed;
    const named = (...names) => options.some((option) => names.includes(option.name));
    const files = options.filter((option) => option.name === 'file').map((option) => option.value); // -f/--file in every form: the file is a destination
    for (const word of files) destinations.push({ word, via: 'git config --file' });
    if (named('edit') || operands[0]?.v === 'edit') return refuseGit('config --edit / config edit runs an editor');
    const writes = named('unset', 'unset-all', 'add', 'replace-all', 'rename-section', 'remove-section');
    const reads = options.some((option) => /^(get|list|show-)/.test(option.name) || option.name === 'name-only') || (operands.length <= 1 && !writes);
    const scopeFlag = named('global', 'system');
    const blob = named('blob'); // read-only: git refuses to write a blob
    if (files.length) return finish(); // an explicit file: the destination check is the whole rule (no repository is involved)
    if (blob) return reads ? finish() : refuseGit('config --blob is read-only');
    if (scopeFlag) return reads ? finish() : refuseGit('config --global/--system writes the user or system configuration, which is not a repository destination');
    if (reads) return finish();
    return writer(); // git config key value, --unset, --add ... write the repository configuration
  }
  if (GIT_READ_ONLY.has(name) || GIT_DIFF_FAMILY.has(name)) {
    if (after.some((word) => GIT_CODE_OPTIONS.test(word.v) || word.v === '-O' || /^-O./.test(word.v))) return refuseGit(`${name}: an option runs an external program (--upload-pack, --exec, -O pager)`);
    if (name === 'reflog' && after.some((word) => word.dyn || ['expire', 'delete'].includes(word.v))) return writer(); // reflog expire|delete rewrite .git/logs
    // status, diff, describe and friends refresh the index opportunistically: .git/index and index.lock are WRITES unless optional locks are off.
    if (gitTouchesIndex(name, after) && !optionalLocksOff) return writer();
    return finish();
  }
  if (GIT_LOCAL_WRITERS.has(name)) return writer();
  return refuseGit(`the subcommand "${name}" is not modelled (it may be an alias, a hook-running command or a writer the guard cannot place)`);
}


function analyzeCommand(items, ctx, state, scope, depth, heavyText) {
  let args = items.map(({ v, dyn, prefix }) => ({ v, dyn, prefix }));
  const { startOp, endOp } = scope.ops ?? {};
  for (let hop = 0; hop < 8; hop += 1) {
    while (args.length && !args[0].dyn && KEYWORDS.has(args[0].v)) args = args.slice(1);
    if (!args.length) return null;
    if (args[0].dyn) {
      for (const word of args.slice(1)) { const found = word.prefix?.startsWith('/') ? protectedWrite({ ...word, dyn: true }, scope, ctx, 'a command with an unevaluated program name', true) : null; if (found) return found; }
      return heavyText ? refuse('the command name is built from a shell expansion, so whether it runs simctl or xcodebuild cannot be determined', SPLIT) : null;
    }
    const program = programName(args[0]); const rest = args.slice(1);
    if (REMOTE.has(program)) return null;
    if (program === 'cd' || program === 'pushd' || program === 'popd') {
      let next = null;
      if (program === 'cd') {
        const target = rest.find((word) => word.v !== '--' && !/^-[LPe@]+$/.test(word.v));
        if (!target) next = cdTarget(ctx.home, null, ctx);
        else if (!target.dyn && target.v !== '-') next = cdTarget(target.v, state.cwd, ctx); // null when logical and physical ".." disagree
      }
      if (endOp === '|' || endOp === '&') return null; // a pipeline stage or background job: its cd never reaches later commands
      if (next && endOp === '&&' && startOp !== '||') { state.cwd = next; state.chainCd = true; } else state.cwd = null;
      return null;
    }
    if (program === 'export' || program === 'declare' || program === 'typeset' || program === 'readonly' || program === 'local') {
      declareBuiltin(program, rest, state, scope, ctx);
      return null;
    }
    // Builtins that change variables or the shell in ways the guard does not follow: forget what it knew.
    if (program === 'read' || program === 'mapfile' || program === 'readarray' || program === 'unset' || program === 'getopts' || program === 'let' || (program === 'printf' && rest.some((word) => word.v === '-v'))) {
      const names = rest.filter((word) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(word.v)).map((word) => word.v);
      for (const name of [...names, 'REPLY', 'MAPFILE', 'OPTARG', 'OPTIND']) state.vars.set(name, DYN);
      if (program === 'let') poison(state);
      return null;
    }
    if (program === 'for' || program === 'select') { state.compound = true; if (rest[0]) state.vars.set(rest[0].v, DYN); return null; }
    if (program === 'case' || program === 'function' || program === 'coproc') { state.compound = true; return null; }
    if (program === 'source' || program === '.') { poison(state); return null; } // the sourced file can set anything
    if (program === 'trap') { // the handler text runs later; analyze it as if it ran now
      const handler = rest.find((word) => !word.v.startsWith('-'));
      if (handler && !handler.dyn && handler.v) return analyzeText(handler.v, ctx, forkState(state, scope.vars, scope.cwd), depth + 1);
      return handler?.dyn ? refuse('trap is given a handler built from a shell expansion', SPLIT) : null;
    }
    if (program === 'env' || program === 'sudo') {
      const parsed = wrapperParse(program, rest);
      if (parsed.error) return refuse(`${program}: ${parsed.error}, so the program it runs cannot be located`, `Run the command directly instead of through ${program} options the guard does not model.`);
      let index = 0;
      const unset = program === 'env' ? parsed.opts.filter((opt) => ['u', 'unset'].includes(opt.name)) : [];
      const chdir = parsed.opts.find((opt) => ['C', 'chdir', 'D'].includes(opt.name) && (program === 'env' || opt.name === 'chdir' || opt.name === 'D'));
      if (parsed.opts.some((opt) => ['i', 'ignore-environment'].includes(opt.name))) scope.vars = new Map(GIT_PROBE_VARS.map((name) => [name, UNSET])); // the child has no HOME/XDG_CONFIG_HOME, which is not the hook's inherited value
      for (const opt of unset) scope.vars.set(opt.value?.v ?? '', DYN);
      // chdir(2) semantics: symlinks in each existing prefix resolve BEFORE any "..", never lexically.
      if (chdir) { const resolved = chdir.value.dyn ? null : absolutePath(chdir.value.v, scope.cwd); scope.cwd = resolved ? physicalDir(resolved, ctx) : null; }
      const operands = parsed.operands;
      for (; index < operands.length; index += 1) {
        const match = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(operands[index].v);
        if (!match) break;
        scope.vars.set(match[1], operands[index].dyn ? DYN : match[2]); markExport(scope, match[1], true);
      }
      args = operands.slice(index); continue;
    }
    if (program === 'xcrun' || Object.hasOwn(WRAPPERS, program)) {
      if ((program === 'command' || program === 'builtin') && rest.length === 0) return null;
      const parsed = wrapperParse(program, rest);
      if (parsed.error) return refuse(`${program}: ${parsed.error}, so the program it runs cannot be located`, `Run the SDK tool directly instead of through ${program} options the guard does not model.`);
      if (parsed.query) return null;
      if (WRAPPERS[program].writes) {
        for (const opt of parsed.opts) if (WRAPPERS[program].writes.includes(opt.name) && opt.value) { const found = protectedWrite(opt.value, scope, ctx, `${program} -o`); if (found) return found; }
      }
      if (program === 'xargs') {
        // stdin supplies words that are appended to, or substituted into, the command. When the command is one the guard
        // tracks (a writer, tar/find/sed/git, xcodebuild/simctl, a wrapper, a shell, an interpreter) those words control its
        // final argv, so the destination cannot be resolved: refuse. A command the guard does not model is passed through.
        const command = parsed.operands[0];
        const tracked = new Set([...Object.keys(WRITERS), ...Object.keys(WRAPPERS), ...INTERPRETERS, 'tar', 'bsdtar', 'gtar', 'find', 'sed', 'gsed', 'git', 'xcodebuild', 'simctl', 'xcrun', 'sh', 'bash', 'zsh', 'dash', 'ksh', 'eval', 'trap', 'sudo', 'env']);
        if (command && (command.dyn || tracked.has(programName(command)))) return refuse(`xargs: stdin controls the final arguments of ${command.dyn ? 'a command built from a shell expansion' : programName(command)}, so its destination cannot be resolved`, 'Run the command directly with literal arguments (a for loop over a literal list), not through xargs.');
      }
      args = parsed.operands; continue;
    }
    if (SHELLS.has(program)) {
      const { script, error } = shellScript(rest);
      if (error) return refuse(`${program}: an option word is built from a shell expansion`, SPLIT);
      if (!script) return null;
      if (script.dyn) return refuse(`the ${program} -c script is built from a shell expansion and cannot be analyzed`, SPLIT);
      return analyzeText(script.v, ctx, forkState(state, scope.vars, scope.cwd), depth + 1); // a child shell: its assignments do not come back
    }
    if (program === 'eval') {
      if (rest.some((word) => word.dyn)) return refuse('eval is given a shell expansion that cannot be analyzed', SPLIT);
      const found = analyzeText(rest.map((word) => word.v).join(' '), ctx, forkState(state, scope.vars, scope.cwd), depth + 1);
      poison(state); // eval runs in THIS shell: whatever it assigned or cd'ed to is not tracked
      return found;
    }
    if (INTERPRETERS.has(program) || /^python[\d.]+$/.test(program)) {
      const inline = rest.some((word) => ['-c', '-e', '-p', '--eval', '--command'].includes(word.v));
      if (inline && rest.some((word) => HEAVY_PROGRAMS.test(word.v))) return refuse(`inline ${program} code runs simctl or xcodebuild, and its write destinations are not visible in the arguments`, 'Run the xcodebuild/simctl command directly with explicit paths.');
      return null;
    }
    if (program === 'simctl') return simctlRule(rest, scope, ctx);
    if (program === 'xcodebuild') return xcodebuildRule(rest, scope, ctx);
    if (program === 'tar' || program === 'bsdtar' || program === 'gtar') return tarRule(rest, scope, ctx);
    if (program === 'find') return findRule(rest, ctx, state, scope, depth, heavyText);
    if (program === 'sed' || program === 'gsed') return sedRule(program, rest, scope, ctx);
    if (program === 'git') return gitRule(rest, scope, ctx);
    if (Object.hasOwn(WRITERS, program)) return writerRule(program, rest, scope, ctx);
    return null;
  }
  return refuse('command wrappers are nested too deeply to analyze', SPLIT);
}

// ---------------------------------------------------------------------------------------------------------------
/** The first violation for one shell command string, or null. `options`: { cwd, home, fs, owners, volumesRoot, agentVolumes, systemRoots }. */
export function heavyCommandViolation(command, options = {}) {
  if (typeof command !== 'string') return null;
  const ctx = context(options);
  try {
    return analyzeText(command, ctx, { vars: new Map(), cwd: ctx.cwd ?? null, home: ctx.home, chainCd: false, compound: false, unknownAll: false }, 0);
  } catch (error) {
    return refuse(`the command could not be analyzed (${error?.message ?? error})`, SPLIT);
  }
}

/** The violation for one file path written by Write/Edit/MultiEdit/apply_patch, or null. */
export function heavyPathViolation(file, options = {}) {
  if (typeof file !== 'string' || !file) return null;
  const ctx = context(options);
  try {
    const expanded = file === '~' ? ctx.home : file.startsWith('~/') ? path.join(ctx.home, file.slice(2)) : file;
    const absolute = absolutePath(expanded.replace(/\\/g, '/'), ctx.cwd);
    if (!absolute) return refuse(`the edit target ${file} is relative and the working directory is not known`, 'Use an absolute path.');
    const hit = protectedHit(absolute, ctx);
    return hit ? violation(hit.rule, `the edit targets ${hit.what}`, requiredFor.path) : null;
  } catch { return refuse(`the edit target ${file} could not be inspected`, 'Use an absolute path.'); }
}

export function heavyWriteDenial(found) {
  return { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: `${HEAVY_WRITE_TAG}\nWhy: [${found.rule}] ${found.why}. Adrian's rule: no agent writes to the Mac internal drive; /Volumes/D is the only agent volume.\nRequired: ${found.required}` } };
}

// ---------------------------------------------------------------------------------------------------------------
const SHELL_TOOLS = new Set(['Bash', 'bash', 'shell', 'Shell', 'exec_command', 'shell_command']);
const QUOTES = new Set(['"', "'", '`']);

/**
 * Decode ONE JavaScript string literal at `from` exactly as the language does: \n \r \t \b \f \v, \0 (not before a digit),
 * \xHH, \uHHHH, \u{H...}, line continuations, and identity escapes. Anything it does not decode with certainty (legacy
 * octal, malformed \x or \u, a raw newline in a quoted string, an unterminated literal) returns null, and the caller refuses.
 * `interpolated` is true when a template literal contains `${`.
 */
function stringLiteralAt(code, from) {
  const quote = code[from];
  if (!QUOTES.has(quote)) return null;
  let value = ''; let interpolated = false;
  for (let i = from + 1; i < code.length; i += 1) {
    const c = code[i];
    if (c === quote) return { value, end: i + 1, interpolated };
    if ((c === '\n' || c === '\r') && quote !== '`') return null;
    if (quote === '`' && c === '\r') { if (code[i + 1] === '\n') i += 1; value += '\n'; continue; }
    if (quote === '`' && c === '$' && code[i + 1] === '{') interpolated = true;
    if (c !== '\\') { value += c; continue; }
    const e = code[i + 1];
    if (e === undefined) return null;
    i += 1;
    if (e === 'n') value += '\n';
    else if (e === 'r') value += '\r';
    else if (e === 't') value += '\t';
    else if (e === 'b') value += '\b';
    else if (e === 'f') value += '\f';
    else if (e === 'v') value += '\v';
    else if (e === '0') { if (/\d/.test(code[i + 1] ?? '')) return null; value += '\0'; }
    else if (/[1-9]/.test(e)) return null; // legacy octal escape / \8 \9
    else if (e === 'x') {
      const hex = /^[0-9a-fA-F]{2}/.exec(code.slice(i + 1, i + 3));
      if (!hex) return null;
      value += String.fromCharCode(parseInt(hex[0], 16)); i += 2;
    } else if (e === 'u') {
      if (code[i + 1] === '{') {
        const close = code.indexOf('}', i + 2);
        const body = close < 0 ? '' : code.slice(i + 2, close);
        if (!/^[0-9a-fA-F]{1,8}$/.test(body) || parseInt(body, 16) > 0x10ffff) return null;
        value += String.fromCodePoint(parseInt(body, 16)); i = close;
      } else {
        const hex = /^[0-9a-fA-F]{4}/.exec(code.slice(i + 1, i + 5));
        if (!hex) return null;
        value += String.fromCharCode(parseInt(hex[0], 16)); i += 4;
      }
    } else if (e === '\r') { if (code[i + 1] === '\n') i += 1; } // line continuation
    else if (e === '\n' || e === ' ' || e === ' ') { /* line continuation */ }
    else value += e; // identity escape (\\ \' \" \` \$ and any other character)
  }
  return null;
}

/** Tokens of composed JavaScript: strings (decoded), identifiers, numbers and single punctuation characters. Null when it cannot be read exactly. */
function jsTokens(code) {
  const tokens = [];
  for (let i = 0; i < code.length;) {
    const c = code[i];
    if (/\s/.test(c)) { i += 1; continue; }
    if (QUOTES.has(c)) {
      const literal = stringLiteralAt(code, i);
      if (!literal) return { error: 'a string literal has an escape or shape the guard does not decode exactly' };
      if (literal.interpolated) return { error: 'a template literal contains ${...} interpolation' };
      tokens.push({ t: 'str', v: literal.value }); i = literal.end; continue;
    }
    if (c === '/') return { error: 'the code contains a comment, regular expression or division' };
    const word = /^[A-Za-z_$][\w$]*|^\d[\w.]*/.exec(code.slice(i));
    if (word) { tokens.push({ t: 'id', v: word[0] }); i += word[0].length; continue; }
    tokens.push({ t: 'p', v: c }); i += 1;
  }
  for (let i = 0; i + 2 < tokens.length; i += 1) if (tokens[i].v === '.' && tokens[i + 1].v === '.' && tokens[i + 2].v === '.' && tokens[i].t === 'p') return { error: 'the code uses spread syntax' };
  return { tokens };
}

/**
 * Every exec_command-style call object in composed functions.exec code: { command, cwd } with each call's OWN workdir.
 * The cmd/command and workdir/cwd values must each be ONE complete string literal; anything else (concatenation, call,
 * identifier, template interpolation, shorthand, spread, computed key, duplicate key) refuses the whole code.
 */
function composedCalls(code, outerCwd, resolveDir) {
  const lexed = jsTokens(code);
  if (lexed.error) return { refuse: lexed.error };
  const { tokens } = lexed; const calls = [];
  const KEYS = ['cmd', 'command', 'workdir', 'cwd'];
  // Only DIRECT calls with inline literals: exec_command must be called with an object literal written in the call, an
  // object built earlier, passed by variable, mutated (p.cmd = ..., p["cmd"] = ...) or merged is refused.
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token.t === 'id' && /exec_command$/.test(token.v) && !(tokens[i + 1]?.v === '(' && tokens[i + 1].t === 'p' && tokens[i + 2]?.v === '{' && tokens[i + 2].t === 'p')) return { refuse: 'exec_command is not called directly with an inline object literal (indirect call, variable argument or alias)' };
    if (token.t === 'p' && token.v === '.' && tokens[i + 1]?.t === 'id' && KEYS.includes(tokens[i + 1].v)) return { refuse: `the code reads or assigns a .${tokens[i + 1].v} property, so a call's arguments may be built or mutated indirectly` };
    if (token.t === 'p' && token.v === '[' && tokens[i + 1]?.t === 'str' && KEYS.includes(tokens[i + 1].v) && tokens[i + 2]?.v === ']') return { refuse: 'the code indexes a cmd/command/workdir/cwd property, so the arguments of a call may be built or mutated indirectly' };
  }
  const closeFor = { '{': '}', '[': ']', '(': ')' };
  const matchClose = (open) => { // index of the token that closes the bracket at tokens[open]
    const stack = [];
    for (let i = open; i < tokens.length; i += 1) {
      const token = tokens[i];
      if (token.t !== 'p') continue;
      if (closeFor[token.v]) stack.push(closeFor[token.v]);
      else if (token.v === '}' || token.v === ']' || token.v === ')') { if (stack.pop() !== token.v) return -1; if (!stack.length) return i; }
    }
    return -1;
  };
  for (let open = 0; open < tokens.length; open += 1) {
    if (tokens[open].t !== 'p' || tokens[open].v !== '{') continue;
    const close = matchClose(open);
    if (close < 0) return { refuse: 'the code has unbalanced brackets' };
    // split the body at top-level commas
    const properties = []; let current = []; let depth = 0;
    for (let i = open + 1; i < close; i += 1) {
      const token = tokens[i];
      if (token.t === 'p' && closeFor[token.v]) depth += 1;
      else if (token.t === 'p' && (token.v === '}' || token.v === ']' || token.v === ')')) depth -= 1;
      if (depth === 0 && token.t === 'p' && token.v === ',') { properties.push(current); current = []; } else current.push(token);
    }
    properties.push(current);
    const found = { cmd: [], command: [], workdir: [], cwd: [] };
    for (const property of properties) {
      if (!property.length) continue;
      const [head, colon] = property;
      if (head.t === 'p' && head.v === '[') return { refuse: 'an object uses a computed property key' };
      const name = head.t === 'str' || head.t === 'id' ? head.v : null;
      if (!name || !Object.hasOwn(found, name)) continue;
      if (!colon || colon.t !== 'p' || colon.v !== ':') return { refuse: `${name} is written as shorthand or a method, so its value cannot be read` };
      const value = property.slice(2);
      if (value.length !== 1 || value[0].t !== 'str') return { refuse: `${name} is not a single complete string literal` };
      found[name].push(value[0].v);
    }
    const commands = [...found.cmd, ...found.command]; const dirs = [...found.workdir, ...found.cwd];
    if (commands.length > 1 || dirs.length > 1) return { refuse: 'an object repeats cmd/command or workdir/cwd' };
    if ((commands.length || dirs.length) && !(tokens[open - 1]?.v === '(' && tokens[open - 2]?.t === 'id' && /exec_command$/.test(tokens[open - 2].v))) return { refuse: 'an object with cmd/command/workdir/cwd is not the inline argument of an exec_command call' };
    if (!commands.length) continue;
    let cwd = outerCwd;
    if (dirs.length) {
      const dir = dirs[0];
      const absolute = dir.startsWith('/') ? dir : outerCwd ? `${outerCwd.replace(/\/+$/, '')}/${dir}` : null;
      cwd = absolute ? resolveDir(absolute) : null;
    }
    calls.push({ command: commands[0], cwd });
  }
  return { calls };
}

function commandsOf(input, cwd, resolveDir) {
  const tool = input?.tool_name ?? input?.toolName ?? '';
  const data = input?.tool_input ?? input?.toolInput ?? {};
  const command = tool === 'exec_command' ? data.cmd ?? input?.cmd : data.command ?? data.cmd ?? data.code ?? data.input ?? input?.command ?? input?.cmd ?? input?.code;
  if (typeof command !== 'string') return { calls: [] };
  const composed = /(?:^|\.)functions[._]exec$/.test(tool) || data.code !== undefined || input?.code !== undefined;
  if (!composed) return SHELL_TOOLS.has(tool) || !tool ? { calls: [{ command, cwd }] } : { calls: [] };
  return composedCalls(command, cwd, resolveDir);
}

/**
 * PreToolUse decision, or null when the guard has no objection. macOS hosts only (`input.platform` overrides
 * `process.platform`, as for the other policy hooks). `options` is the test seam passed to the checks above.
 */
export function heavyWriteHookDecision(input, options = {}) {
  const platform = options.platform ?? input?.platform ?? process.platform;
  if (platform !== 'darwin') return null;
  const tool = input?.tool_name ?? input?.toolName ?? '';
  const data = input?.tool_input ?? input?.toolInput ?? {};
  const base = typeof input?.cwd === 'string' && input.cwd ? input.cwd : process.cwd();
  const declared = [data.workdir, data.cwd].find((value) => typeof value === 'string' && value);
  // A tool workdir is handed to chdir(2): resolve symlinks of each existing prefix BEFORE applying "..".
  const resolveDir = (absolute) => { const resolved = physicalResolve(absolute, options.fs ?? realFs); return resolved.error ? null : resolved.real; };
  const cwd = declared ? resolveDir(declared.startsWith('/') ? declared : `${base.replace(/\/+$/, '')}/${declared}`) : base;
  const scoped = { ...options, cwd };
  let found = null;
  if (['Write', 'Edit', 'MultiEdit'].includes(tool)) {
    found = heavyPathViolation(data.file_path ?? data.filePath ?? data.path ?? data.file, scoped);
  } else if (tool === 'apply_patch') {
    const patch = data.patch ?? data.input ?? input?.patch ?? data.code;
    if (typeof patch === 'string') {
      for (const match of patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$|^\*\*\* Move to: (.+)$/gm)) {
        found = heavyPathViolation((match[1] ?? match[2]).trim(), scoped);
        if (found) break;
      }
    }
  } else {
    const extracted = commandsOf(input, cwd, resolveDir);
    if (extracted.refuse) found = refuse(`functions.exec code: ${extracted.refuse}`, 'Call exec_command with cmd (and workdir) each written as ONE plain string literal, or run the command through the shell tool.');
    else for (const call of extracted.calls) { found = heavyCommandViolation(call.command, { ...options, cwd: call.cwd }); if (found) break; }
  }
  return found ? heavyWriteDenial(found) : null;
}
