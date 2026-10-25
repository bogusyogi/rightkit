import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { posix, join } from 'node:path';
import test from 'node:test';
import { pathToFileURL } from 'node:url';
import { installPolicyAssets } from './install-policy-assets.mjs';
import { setAllowlistSources, setScopeDependencies, ownershipWriteDecision } from './ownership-write-hook.mjs';
import { HEAVY_WRITE_RULES, HEAVY_WRITE_TAG, gitChildEnv, heavyCommandViolation, heavyPathViolation, heavyWriteHookDecision, loadAppOwners, realFs } from './agent-heavy-write-guard.mjs';

setScopeDependencies({ git: () => null, exists: () => false, roots: [] });
setAllowlistSources({ readTrust: () => null, shipped: null });

// A virtual macOS volume layout. "/" and the Data volume are internal; /Volumes/D is the only AGENT volume. /Volumes/E
// is a real external mount that is Adrian's personal volume. /Volumes/D-evil is a plain directory on the system volume
// (prefix trick), "Macintosh HD" is the usual symlink to "/".
// Every counterexample string below is DATA ONLY: it is parsed, never executed.
const home = '/Users/adrian';
function fakeFs({ dirs, links = {}, mounts, files = {} }) {
  const known = new Set(['/']);
  for (const dir of [...dirs, ...Object.keys(files)]) for (let at = dir; at !== '/'; at = posix.dirname(at)) known.add(at);
  const error = (code) => Object.assign(new Error(code), { code });
  const realpath = (input) => {
    let resolved = '/'; const queue = input.split('/').filter(Boolean); let hops = 0;
    while (queue.length) {
      const next = posix.join(resolved, queue.shift());
      if (links[next]) { if ((hops += 1) > 20) throw error('ELOOP'); queue.unshift(...links[next].split('/').filter(Boolean)); resolved = '/'; continue; }
      if (!known.has(next)) throw error('ENOENT');
      resolved = next;
    }
    return resolved;
  };
  return {
    kind(target) {
      if (links[target]) return 'symlink';
      try { const real = realpath(target); return Object.hasOwn(files, real) ? 'file' : known.has(real) ? 'dir' : null; } catch (caught) { if (caught.code === 'ENOENT') return null; throw caught; }
    },
    realpath,
    readFile(target) {
      const text = Object.hasOwn(files, target) ? files[target] : undefined;
      if (typeof text !== 'string') throw error(links[target] || known.has(target) ? 'EINVAL' : 'ENOENT');
      return text;
    },
    device(target) {
      let best; let length = -1;
      for (const [mount, device] of Object.entries(mounts)) if ((target === mount || target.startsWith(mount === '/' ? '/' : `${mount}/`)) && mount.length > length) { best = device; length = mount.length; }
      return best;
    },
  };
}
// Run roots under /Volumes/D/scratch (the lease file is `.rightkit-run.json`, as `right-release disk run-root <name>` writes it).
const LEASE = '.rightkit-run.json';
const lease = (name, until = '2999-01-01T00:00:00.000Z', extra = {}) => JSON.stringify({ schema: 1, name, createdAt: '2026-10-01T00:00:00.000Z', leaseUntil: until, ...extra });
const scratch = '/Volumes/D/scratch';
const fs = fakeFs({
  files: {
    [`${scratch}/run1/${LEASE}`]: lease('run1'),
    [`${scratch}/run1/sub/keep.txt`]: 'x',
    [`${scratch}/run-expired/${LEASE}`]: lease('run-expired', '2020-01-01T00:00:00.000Z'),
    [`${scratch}/run-garbage/${LEASE}`]: '{not json',
    [`${scratch}/run-schema2/${LEASE}`]: lease('run-schema2', undefined, { schema: 2 }),
    [`${scratch}/run-noname/${LEASE}`]: JSON.stringify({ schema: 1, createdAt: '2026-10-01T00:00:00.000Z', leaseUntil: '2999-01-01T00:00:00.000Z' }),
    [`${scratch}/run-baddate/${LEASE}`]: lease('run-baddate', 'tomorrow'),
    [`${scratch}/run-numdate/${LEASE}`]: lease('run-numdate', 32503680000000),
    [`${scratch}/run-null/${LEASE}`]: 'null',
    [`${scratch}/run-empty/${LEASE}`]: '',
  },
  dirs: [
    `${scratch}/run2`, `${scratch}/run1-evil`, `${scratch}/run-dirlease/${LEASE}`, `${scratch}/run-linklease`, `${scratch}/run-escape`,
    '/System/Volumes/Data', '/tmp', `${home}/work`, `${home}/Library/Developer/CoreSimulator/Devices`, `${home}/Library/Caches/cutright`,
    `${home}/Library/Caches/SomethingElse`, `${home}/Library/Application Support/Orthic Labs/shared/models`,
    '/Volumes/D/scratch', '/Volumes/D/scratch/hasgit/.git', '/Volumes/D/scratch/fresh', '/Volumes/D/claude/app', '/Volumes/D/caches', '/Volumes/D/.xcode/DerivedData', '/Volumes/D-evil', '/Volumes/E',
  ],
  links: {
    [`${scratch}/run-symrun`]: `${scratch}/run1`, // a run directory that is a symlink to a leased one
    [`${scratch}/run-linklease/${LEASE}`]: `${scratch}/run1/${LEASE}`, // a lease that is a symlink to a live lease
    [`${scratch}/run-escape/out`]: '/Volumes/D/claude/app', // a link inside a (not yet leased) run root
    [`${scratch}/run1/to-claude`]: '/Volumes/D/claude/app', // a link inside a leased run root that leaves it
    [`${scratch}/run1/to-run2`]: `${scratch}/run2`,
    [`${scratch}/run1/dangling`]: '/Volumes/D/nowhere',
    '/Volumes/Macintosh HD': '/',
    '/Volumes/D/scratch/to-internal': `${home}/work`,
    '/Volumes/D/scratch/dangling': `${home}/nowhere`,
    '/Volumes/D/scratch/dangling-cache': `${home}/Library/Caches/cutright/new`,
    [`${home}/Library/Caches/heardright`]: '/Volumes/D/caches',
  },
  mounts: { '/': 1, '/System/Volumes/Data': 2, '/Users': 2, '/Volumes/D': 3, '/Volumes/E': 4 },
});
// A fake git for the bounded `git rev-parse` / `git remote -v` seam. By default every cwd is its own repository (work tree,
// .git and common dir inside it) with one network remote; `repos[cwd]` overrides top/gitDir/common/remotes.
function fakeGit({ repos = {}, status = 0, throws = null, out = null, remoteStatus = 0 } = {}) {
  // repos[cwd] may also set worktrees (paths `git worktree list --porcelain` prints) and notRepo (rev-parse exits 128)
  const calls = [];
  const runner = (args, options) => {
    calls.push({ args, ...options });
    if (throws) throw throws;
    const base = { top: options.cwd, gitDir: `${options.cwd}/.git`, common: `${options.cwd}/.git`, remotes: [['origin', 'https://github.com/orthic/x.git', 'https://github.com/orthic/x.git']] };
    const repo = { ...base, ...(repos[options.cwd] ?? {}) };
    if (args[0] === 'worktree') return { status: 0, stdout: (repo.worktrees ?? [repo.top]).map((root) => `worktree ${root}\nHEAD 0000\nbranch refs/heads/main\n\n`).join('') };
    if (args.includes('rev-parse') && repo.notRepo) return { status: 128, stdout: '' };
    if (args.includes('rev-parse')) return status === 0 ? { status: 0, stdout: out ?? `${repo.top}
${repo.gitDir}
${repo.common}
` } : { status, stdout: '' };
    if (args[0] === 'remote') return remoteStatus === 0 ? { status: 0, stdout: repo.remotes.map(([name, fetchUrl, pushUrl]) => `${name}	${fetchUrl} (fetch)
${name}	${pushUrl} (push)
`).join('') } : { status: remoteStatus, stdout: '' };
    return { status: 1, stdout: '' };
  };
  runner.calls = calls;
  return runner;
}
const seam = (cwd = '/Volumes/D/claude/app', extra = {}) => ({ fs, home, cwd, env: {}, gitRunner: fakeGit(), ...extra });
const check = (command, cwd, extra) => heavyCommandViolation(command, seam(cwd, extra));
const denied = (command, rule, cwd, extra) => {
  const found = check(command, cwd, extra);
  assert.ok(found, `expected a denial for: ${command}`);
  if (rule) assert.equal(found.rule, rule, `${command}: ${found.why}`);
  assert.match(found.required, /\/Volumes\/D/);
  return found;
};
const allowed = (command, cwd, extra) => assert.equal(check(command, cwd, extra), null, `expected allow for: ${command}\n${JSON.stringify(check(command, cwd, extra))}`);
const ext = '/Volumes/D/scratch/run1';
const dd = '/Volumes/D/.xcode/DerivedData';
const device = 'com.apple.CoreSimulator.SimDeviceType.iPhone-16';
const cache = `${home}/Library/Caches/cutright`;
const hook = (tool_name, tool_input, cwd = home) => heavyWriteHookDecision({ tool_name, tool_input, cwd, platform: 'darwin' }, seam(cwd));

test('rules are named and each denial names the agent-volume alternative', () => {
  assert.deepEqual(HEAVY_WRITE_RULES.map((rule) => rule.id), ['simctl-no-simulators', 'xcodebuild-derived-data', 'xcodebuild-output', 'internal-heavy-path', 'non-agent-volume', 'undeterminable-destination', 'repo-create-outside-run-root', 'download-outside-run-root']);
  const decision = hook('Bash', { command: 'xcrun simctl create A t' });
  assert.equal(decision.hookSpecificOutput.permissionDecision, 'deny');
  const reason = decision.hookSpecificOutput.permissionDecisionReason;
  assert.ok(reason.startsWith(HEAVY_WRITE_TAG));
  assert.match(reason, /shared iPhone 18 Pro/);
  assert.match(reason, /\/Volumes\/D\/\.xcode\/DerivedData/);
  assert.match(reason, /\/Volumes\/E is Adrian's personal volume/);
});

test('host policy: simctl create and clone are always denied (no simulators); other simctl operations pass', () => {
  denied(`xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`xcrun simctl clone A B`, 'simctl-no-simulators');
  denied(`xcrun simctl --set ${ext}/sims create A ${device}`, 'simctl-no-simulators'); // even with an agent-volume set
  denied(`SET=${ext}/sims; xcrun simctl --set $SET create A ${device}`, 'simctl-no-simulators');
  denied(`xcrun --sdk iphonesimulator simctl create A ${device}`, 'simctl-no-simulators');
  denied(`bash -lc "xcrun simctl create A ${device}"`, 'simctl-no-simulators');
  denied(`eval 'xcrun simctl create A ${device}'`, 'simctl-no-simulators');
  denied(`echo $(xcrun simctl create A ${device})`, 'simctl-no-simulators');
  denied(`xcrun simctl --set ~/Library/Developer/CoreSimulator/Devices boot A`, 'internal-heavy-path');
  denied(`xcrun simctl --set /Volumes/E/sims boot A`, 'non-agent-volume');
  allowed('xcrun simctl list devices');
  allowed(`xcrun simctl --set ${ext}/sims list`);
  allowed('xcrun simctl boot A && xcrun simctl launch A com.example.app');
});

test('xcodebuild build, test and archive need -derivedDataPath on the agent volume; read-only queries pass', () => {
  denied('xcodebuild -scheme App build', 'xcodebuild-derived-data');
  denied('xcodebuild -scheme App test', 'xcodebuild-derived-data');
  denied('xcodebuild -scheme App', 'xcodebuild-derived-data');
  denied('xcrun xcodebuild -scheme App build-for-testing', 'xcodebuild-derived-data');
  denied(`env CI=1 xcodebuild -scheme App archive -archivePath ${ext}/App.xcarchive`, 'xcodebuild-derived-data');
  denied('xcodebuild -scheme App build -derivedDataPath /Users/adrian/dd', 'xcodebuild-derived-data');
  allowed(`xcodebuild -scheme App build -derivedDataPath ${dd}`);
  allowed(`xcodebuild -scheme App test -derivedDataPath ${dd} -resultBundlePath ${ext}/r.xcresult`);
  allowed(`xcodebuild -scheme App archive -derivedDataPath ${dd} -archivePath ${ext}/App.xcarchive`);
  allowed('xcodebuild -scheme App build -derivedDataPath build/dd'); // cwd /Volumes/D/claude/app is on the agent volume
  denied('xcodebuild -scheme App build -derivedDataPath build/dd', 'xcodebuild-derived-data', `${home}/work`);
  denied(`cd ${home}/work && xcodebuild -scheme App build -derivedDataPath build/dd`, 'xcodebuild-derived-data');
  allowed(`cd ${ext} && xcodebuild -scheme App build -derivedDataPath build/dd`, `${home}/work`);
  for (const query of ['-list', '-list -json', '-showsdks', '-version', '-help', '-usage', '-showBuildSettings -scheme App', '-showdestinations -scheme App', '-showTestPlans -scheme App']) allowed(`xcodebuild ${query}`);
  allowed(`xcodebuild -list > ${ext}/projects.txt`);
  allowed('xcodebuild clean -scheme App');
});

test('xcodebuild extra outputs and build-setting overrides must also be on the agent volume when present', () => {
  denied(`xcodebuild -scheme App build -derivedDataPath ${dd} -resultBundlePath /Users/adrian/r.xcresult`, 'xcodebuild-output');
  denied(`xcodebuild -scheme App archive -derivedDataPath ${dd} -archivePath /Users/adrian/Library/Developer/Xcode/Archives/A.xcarchive`, 'xcodebuild-output');
  denied(`xcodebuild -scheme App archive -derivedDataPath ${dd}`, 'xcodebuild-output');
  denied(`xcodebuild -scheme App build -derivedDataPath ${dd} -clonedSourcePackagesDirPath /Users/adrian/pkgs`, 'xcodebuild-output');
  denied(`xcodebuild -scheme App build -derivedDataPath ${dd} SYMROOT=/Users/adrian/work/sym`, 'xcodebuild-output');
  allowed(`xcodebuild -scheme App build -derivedDataPath ${dd} SYMROOT=${ext}/sym OBJROOT=${ext}/obj CODE_SIGNING_ALLOWED=NO`);
  denied('xcodebuild -exportArchive -archivePath a.xcarchive -exportOptionsPlist o.plist', 'xcodebuild-output');
  denied('xcodebuild -exportArchive -archivePath a.xcarchive -exportPath /Users/adrian/out', 'xcodebuild-output');
  allowed(`xcodebuild -exportArchive -archivePath a.xcarchive -exportPath ${ext}/out`);
});

test('host policy: /Volumes/D is the only agent volume; /Volumes/E (personal) is denied everywhere', () => {
  denied('xcodebuild -scheme App build -derivedDataPath /Volumes/E/dd', 'xcodebuild-derived-data');
  denied(`xcodebuild -scheme App build -derivedDataPath ${dd} -resultBundlePath /Volumes/E/r.xcresult`, 'xcodebuild-output');
  denied('cp x /Volumes/E/y', 'non-agent-volume');
  denied('touch /Volumes/E/a /Volumes/E/b', 'non-agent-volume');
  denied('mkdir -p /Volumes/E/new/dir', 'non-agent-volume');
  denied('echo x > /Volumes/E/log.txt', 'non-agent-volume');
  denied('rsync -a ./out/ /Volumes/E/backup/', 'non-agent-volume');
  denied('tar -xf a.tar -C /Volumes/E/x', 'non-agent-volume');
  denied('tar -cf /Volumes/E/a.tar -C /Volumes/D/scratch .', 'non-agent-volume');
  denied('curl -o /Volumes/E/m.bin https://example.com/m.bin', 'non-agent-volume');
  denied('cp x /Volumes/D-evil/y', 'non-agent-volume');
  denied('cd /Volumes/E && touch y', 'non-agent-volume', home, undefined); // the cd itself is fine; the write lands on E
  assert.equal(hook('Write', { file_path: '/Volumes/E/notes.md', content: 'x' }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(hook('Write', { file_path: `${ext}/notes.md`, content: 'x' }), null);
  allowed('cat /Volumes/E/readme.txt'); // reads are not writes
  // The agent volume list is host configuration, not a hard-coded "any external mount" allowance.
  allowed('cp x /Volumes/E/y', undefined, { agentVolumes: ['/Volumes/D', '/Volumes/E'] });
  allowed('xcodebuild -scheme App build -derivedDataPath /Volumes/E/dd', undefined, { agentVolumes: ['/Volumes/D', '/Volumes/E'] });
  denied('cp x /Volumes/D/y', 'non-agent-volume', undefined, { agentVolumes: ['/Volumes/E'] });
  const saved = process.env.RIGHTKIT_AGENT_VOLUMES;
  try {
    process.env.RIGHTKIT_AGENT_VOLUMES = '/Volumes/E';
    denied('cp x /Volumes/D/y', 'non-agent-volume');
    allowed('cp x /Volumes/E/y');
  } finally { if (saved === undefined) delete process.env.RIGHTKIT_AGENT_VOLUMES; else process.env.RIGHTKIT_AGENT_VOLUMES = saved; }
});

test('prefix, traversal, symlink and non-mount tricks are not the agent volume', () => {
  const build = (dir) => `xcodebuild -scheme App build -derivedDataPath "${dir}"`;
  denied(build('/Volumes/D-evil/dd'), 'xcodebuild-derived-data'); // a real directory, but not an agent volume
  denied(build('/Volumes/Dd/dd'), 'xcodebuild-derived-data');
  denied(build('/Volumes/D/../../Users/adrian/dd'), 'xcodebuild-derived-data');
  denied(build('/Volumes/D/scratch/../../../Users/adrian/dd'), 'xcodebuild-derived-data');
  denied(build('/Volumes/D/scratch/to-internal/dd'), 'xcodebuild-derived-data'); // symlink to internal
  denied(build('/Volumes/D/scratch/to-internal'), 'xcodebuild-derived-data');
  denied(build('/Volumes/D/scratch/dangling/dd'), 'xcodebuild-derived-data');
  denied(build('/Volumes/Macintosh HD/Users/adrian/dd'), 'xcodebuild-derived-data'); // symlink to "/"
  denied(build('/Volumes/Missing/dd'), 'xcodebuild-derived-data'); // no such mount
  denied(build('/Volumes'), 'xcodebuild-derived-data');
  denied(build('/Users/adrian/Library/Developer/Xcode/DerivedData'), 'xcodebuild-derived-data');
  denied(build('/Volumes/D/scratch/../scratch/dd'), 'xcodebuild-derived-data'); // even harmless ".." is refused
  allowed(build('/Volumes/D/scratch/never-created/deeper/dd')); // new directories under the agent volume are fine
  allowed(build('//Volumes//D///scratch/./dd'));
  allowed(build(dd));
  // A dangling symlink on the agent volume whose target is a protected cache is refused for ordinary writers too.
  denied('touch /Volumes/D/scratch/dangling-cache/x', 'undeterminable-destination');
  denied('cp x /Volumes/D/scratch/../../../Users/adrian/Library/Caches/cutright/y', 'internal-heavy-path');
});

test('a heavy command whose destination cannot be determined is refused, not guessed', () => {
  denied('xcodebuild -scheme App build -derivedDataPath "$DD"', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath $(mktemp -d /Volumes/D/scratch/dd.XXXX)', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath `mktemp -d`', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath /Volumes/D/scratch/*', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath /Volumes/D/{a,b}', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath "/Volumes/D/scratch/dd', 'undeterminable-destination'); // unclosed quote
  denied('$XCB -scheme App build -derivedDataPath /Volumes/D/scratch/dd # xcodebuild', 'undeterminable-destination');
  denied('python3 -c "import subprocess; subprocess.run([\'xcodebuild\', \'build\'])"', 'undeterminable-destination');
  denied('xcodebuild -scheme App build -derivedDataPath build/dd', 'undeterminable-destination', null);
  denied('for d in a b; do xcodebuild -scheme App build -derivedDataPath $d; done', 'undeterminable-destination');
  denied('X=xcodebuild; $X -scheme App build', 'xcodebuild-derived-data'); // a literal variable is followed
  denied('echo "unterminated', 'undeterminable-destination'); // any unparseable command is refused, not skipped
});

test('redirect destinations are not mistaken for heavy destinations (the dependency tokenizer discards them)', () => {
  denied(`xcodebuild -scheme App build > ${dd}`, 'xcodebuild-derived-data');
  denied(`xcodebuild -scheme App build -derivedDataPath > ${dd}`, 'undeterminable-destination');
  denied(`xcrun simctl create A ${device} > ${ext}/sims`, 'simctl-no-simulators');
  allowed(`xcodebuild -scheme App build -derivedDataPath ${dd} > ${ext}/build.log 2>&1`);
  allowed(`xcodebuild -scheme App build -derivedDataPath ${dd} 2>&1 | tee ${ext}/build.log`);
  allowed(`cat > ${ext}/notes.md <<'EOF'\nrun xcrun simctl create A t\nxcodebuild build\nEOF`);
  allowed('echo xcodebuild build');
  allowed('grep -rn simctl docs');
  allowed('command -v xcodebuild && which xcodebuild');
  allowed('ssh mac "xcrun simctl create A t; xcodebuild build"');
  allowed('git commit -m "document xcodebuild build and simctl create"');
});

test('writes into internal heavy locations are denied; reads and agent-volume destinations pass', () => {
  denied('cp model.bin ~/Library/Caches/cutright/model.bin', 'internal-heavy-path');
  denied('mkdir -p ~/Library/Caches/cutright-engine/x', 'internal-heavy-path');
  denied('mkdir -p ~/Library/Caches/app.orthiclabs.cutright/x', 'internal-heavy-path');
  denied('touch $HOME/Library/Developer/CoreSimulator/Devices/FOO/data', 'internal-heavy-path');
  denied('touch ~/Library/Developer/CoreSimulator/Devices/$UDID/data', 'internal-heavy-path'); // static internal prefix
  denied('echo x > ~/Library/Developer/CoreSimulator/Devices/log.txt', 'internal-heavy-path');
  denied('echo x >> "$HOME/Library/Caches/cutright/log.txt"', 'internal-heavy-path');
  denied('mkdir -p "$HOME/Library/Application Support/Orthic Labs/shared/models/whisper"', 'internal-heavy-path');
  denied('rsync -a ./models/ ~/Library/Application\\ Support/Orthic\\ Labs/shared/models/', 'internal-heavy-path');
  denied('curl -L -o ~/Library/Application\\ Support/Orthic\\ Labs/shared/models/m.bin https://example.com/m.bin', 'internal-heavy-path');
  denied('tar -xf a.tgz -C ~/Library/Developer/CoreSimulator/Devices', 'internal-heavy-path');
  denied('cd ~/Library/Caches && mkdir cutright-new', 'internal-heavy-path');
  denied('cp x /Volumes/D/scratch/to-internal/../../../Users/adrian/Library/Caches/cutright/x', 'internal-heavy-path'); // ".." after a symlink
  allowed('ls -la ~/Library/Caches/cutright ~/Library/Developer/CoreSimulator/Devices');
  allowed('du -sh ~/Library/Caches/cutright');
  allowed('cp ~/Library/Caches/cutright/a /Volumes/D/scratch/a');
  allowed('cp x ~/Library/Caches/SomethingElse/y'); // not a known app owner
  allowed(`cp x ${ext}/models/y && echo done > ${ext}/log`);
  allowed('cp x ~/Library/Caches/heardright/y'); // symlink to /Volumes/D/caches: the real destination is on the agent volume
  denied('cp $SRC $DST', 'undeterminable-destination'); // a wholly dynamic word could be an option such as "-t /internal": refused
  denied('cp x /Volumes/D/scratch/$NAME', 'undeterminable-destination'); // G1: a safe-looking prefix followed by an unresolved part is refused
});

test('Write, Edit and apply_patch targets are checked the same way', () => {
  const run = (tool_name, tool_input) => hook(tool_name, tool_input);
  assert.equal(run('Write', { file_path: `${cache}/x.bin`, content: 'x' }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(run('Edit', { file_path: `${home}/Library/Developer/CoreSimulator/Devices/x/y` }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(run('Edit', { file_path: '~/Library/Developer/CoreSimulator/Devices/x/y' }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(run('Write', { file_path: `${ext}/x.bin` }), null);
  assert.equal(run('Write', { file_path: `${home}/work/x.rs` }), null);
  assert.equal(run('Write', { file_path: `${home}/Library/Caches/SomethingElse/x` }), null);
  assert.equal(run('apply_patch', { patch: `*** Begin Patch\n*** Add File: ${cache}/x\n+hi\n*** End Patch` }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(run('apply_patch', { patch: `*** Begin Patch\n*** Update File: ${home}/work/a.rs\n*** Move to: ${home}/Library/Application Support/Orthic Labs/shared/models/a.rs\n*** End Patch` }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(run('Read', { file_path: `${cache}/x` }), null);
  assert.equal(heavyPathViolation(`${cache}/x`, seam(home)).rule, 'internal-heavy-path');
});

// ------------------------------------------------------------------------------------------------------------------
// #800 review round: seven false allows/denies. Counterexamples are data only.

test('fix 1: no textual prefilter; every write destination and cwd is resolved before the decision', () => {
  denied('cd ~ && cp x Library/Caches/cutright/y', 'internal-heavy-path'); // no "/Library" substring anywhere
  denied('cd "$HOME" && mkdir -p Library/Developer/CoreSimulator/Devices/A', 'internal-heavy-path');
  denied('cd ~/"Libr"ary && touch Caches/cutright/z', 'internal-heavy-path');
  denied('X=Lib; Y=rary; touch ~/$X$Y/Caches/cutright/z', 'internal-heavy-path'); // pieces assembled by the shell
  denied('touch ~/"Lib"rary/Caches/"cut"right/z', 'internal-heavy-path');
  denied('cd /Users/adrian/Library && echo x > Caches/cutright/z', 'internal-heavy-path');
  denied('cp x ./Caches/cutright/y', 'internal-heavy-path', `${home}/Library`);
  denied('echo hi > c/y', 'internal-heavy-path', cache); // relative redirect inside the protected cache
  denied('touch note', 'undeterminable-destination', null); // relative destination, unknown cwd: refused, not skipped
  denied('tee out.log', 'undeterminable-destination', null);
  assert.equal(heavyPathViolation('Caches/cutright/x', seam(`${home}/Library`)).rule, 'internal-heavy-path'); // relative edit target
  assert.equal(heavyPathViolation('x.txt', seam(null)).rule, 'undeterminable-destination');
  allowed('cd /Volumes/D/claude/app && touch note');
});

test('fix 2: attached short options parse like their separated forms', () => {
  const internalCurl = [
    `curl -o"$HOME/Library/Caches/cutright/x" https://example.com/x`,
    'curl -o/Users/adrian/Library/Caches/cutright/x https://example.com/x',
    'curl -sSLo/Users/adrian/Library/Caches/cutright/x https://example.com/x',
    'curl -sSLo /Users/adrian/Library/Caches/cutright/x https://example.com/x',
    'curl --output=/Users/adrian/Library/Caches/cutright/x https://example.com/x',
    'curl --output /Users/adrian/Library/Caches/cutright/x https://example.com/x',
    'curl --out /Users/adrian/Library/Caches/cutright/x https://example.com/x', // unique-prefix abbreviation
    'curl -O --output-dir /Users/adrian/Library/Caches/cutright https://example.com/x',
    'curl --output-dir=/Users/adrian/Library/Caches/cutright -O https://example.com/x',
    'curl -D/Users/adrian/Library/Caches/cutright/h https://example.com/x',
  ];
  for (const command of internalCurl) denied(command, 'internal-heavy-path');
  allowed(`curl -o"${ext}/x" https://example.com/x`);
  allowed(`curl -sSLo${ext}/x https://example.com/x`);
  allowed('curl -o /dev/null https://example.com/x');
  denied(`cd ${cache} && curl -O https://example.com/x`, 'internal-heavy-path');
  denied('curl -O https://example.com/x', 'internal-heavy-path', cache); // -O writes into the working directory
  allowed('curl -O https://example.com/x', `${ext}`);
  denied('cp -t"$HOME/Library/Caches/cutright" a b', 'internal-heavy-path');
  denied('cp -t /Users/adrian/Library/Caches/cutright a b', 'internal-heavy-path');
  denied('cp --target-directory=/Users/adrian/Library/Caches/cutright a b', 'internal-heavy-path');
  denied('cp --target-dir=/Users/adrian/Library/Caches/cutright a b', 'internal-heavy-path');
  denied(`cp -t ${ext} ${cache}/a ${cache}/b`, 'internal-heavy-path'); // GNU -t makes the last operand a source, BSD cp makes it the destination: both are checked
  denied('unzip a.zip -d"$HOME/Library/Developer/CoreSimulator/Devices"', 'internal-heavy-path');
  denied('unzip a.zip -d /Users/adrian/Library/Developer/CoreSimulator/Devices', 'internal-heavy-path');
  denied('unzip a.zip', 'internal-heavy-path', cache); // extracts into the working directory
  allowed(`unzip -l a.zip`, cache);
  denied('wget -O/Users/adrian/Library/Caches/cutright/x https://example.com/x', 'internal-heavy-path');
  denied('wget -P"$HOME/Library/Caches/cutright" https://example.com/x', 'internal-heavy-path');
  denied('wget --directory-prefix=/Users/adrian/Library/Caches/cutright https://example.com/x', 'internal-heavy-path');
  denied('wget https://example.com/x', 'internal-heavy-path', cache);
  allowed('wget -O - https://example.com/x');
  denied('tar -xf a.tar -C"$HOME/Library/Caches/cutright"', 'internal-heavy-path');
  denied('tar -xf a.tar --directory=/Users/adrian/Library/Caches/cutright', 'internal-heavy-path');
  denied('tar -xf a.tar --dir=/Users/adrian/Library/Caches/cutright', 'internal-heavy-path');
  denied('tar -cf"$HOME/Library/Caches/cutright/a.tar" -C /Volumes/D/scratch .', 'internal-heavy-path');
  denied('rsync -a --temp-dir=/Users/adrian/Library/Caches/cutright src/ /Volumes/D/scratch/x/', 'internal-heavy-path');
  denied('rsync -aT/Users/adrian/Library/Caches/cutright src/ /Volumes/D/scratch/x/', 'internal-heavy-path');
  allowed('rsync -a /Users/adrian/Library/Caches/cutright/ host:/backup/'); // remote destination
});

test('fix 3: prefix assignments reach only the child environment; argv expands with the previous shell values', () => {
  denied(`DD=/Users/adrian/dd xcodebuild -scheme App build -derivedDataPath "$DD"`, 'undeterminable-destination');
  denied(`DD=${dd} xcodebuild -scheme App build -derivedDataPath "$DD"`, 'undeterminable-destination'); // refused whatever the value
  denied(`DD=/Users/adrian/dd xcodebuild -scheme App build -derivedDataPath \${DD}/x`, 'undeterminable-destination');
  denied(`DD=${dd} cp x "$DD/y"`, 'undeterminable-destination');
  denied(`HOME=${ext} cp x ~/Library/Caches/cutright/y`, 'undeterminable-destination'); // "~" is $HOME, expanded first
  denied(`A=/x B=$A xcodebuild -scheme App build`, 'undeterminable-destination'); // later assignment reads an earlier one
  denied(`DD=${dd} echo x > "$DD/log"`, 'undeterminable-destination');
  // env: the shell has already expanded argv, so "$DD" is the shell's value, not env's.
  denied(`DD=/Users/adrian/dd; env DD=${dd} xcodebuild -scheme App build -derivedDataPath "$DD"`, 'xcodebuild-derived-data');
  denied(`env DD=${dd} xcodebuild -scheme App build -derivedDataPath "$DD"`, 'undeterminable-destination'); // DD is unset in the shell
  allowed(`DD=${dd}; env DD=/Users/adrian/dd xcodebuild -scheme App build -derivedDataPath "$DD"`);
  allowed(`DD=${dd}; FOO=1 xcodebuild -scheme App build -derivedDataPath "$DD"`);
  // A child shell does see the prefix assignment in its environment.
  denied(`DD=/Users/adrian/dd sh -c 'xcodebuild -scheme App build -derivedDataPath "$DD"'`, 'xcodebuild-derived-data');
  allowed(`DD=${dd} sh -c 'xcodebuild -scheme App build -derivedDataPath "$DD"'`);
  allowed(`CODE_SIGNING_ALLOWED=NO xcodebuild -scheme App build -derivedDataPath ${dd}`);
});

test('fix 4: a cd is certain only inside its && chain; otherwise the working directory is unknown', () => {
  const build = 'xcodebuild -scheme App build -derivedDataPath dd';
  denied(`cd /Volumes/D/nonexistent; ${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/claude/app; ${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/claude/app\n${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/nonexistent || true; ${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/claude/app || exit 1; ${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/claude/app && true; ${build}`, 'undeterminable-destination'); // chain ended: the cd may have failed
  denied(`true || cd /Volumes/D/claude/app && ${build}`, 'undeterminable-destination'); // cd skipped when true succeeds
  denied(`cd /Volumes/D/claude/app; echo x > out.log`, 'undeterminable-destination');
  denied(`pushd /Volumes/D/claude/app && ${build}`, 'undeterminable-destination');
  denied(`cd "$WHERE" && ${build}`, 'undeterminable-destination');
  denied(`cd /Volumes/D/claude/app | ${build}`, 'xcodebuild-derived-data', `${home}/work`); // a pipeline stage's cd never reaches later commands
  allowed(`cd -P /Volumes/D/claude/app && ${build}`, `${home}/work`);
  denied(`cd -P /Volumes/D/scratch/to-internal/.. && ${build}`, 'undeterminable-destination'); // logical and physical ".." disagree
  allowed(`cd /Volumes/D/claude/app && ${build}`, `${home}/work`);
  allowed(`false && cd /Volumes/D/claude/app && ${build}`, `${home}/work`);
  allowed(`(cd /Volumes/D/claude/app && ${build})`, `${home}/work`);
  allowed(`cd /Volumes/D/nonexistent && ${build}`); // cd fails: the build never runs
  allowed(`cd /Users/adrian/work; xcodebuild -scheme App build -derivedDataPath ${dd}`); // absolute destinations never depend on the cd
  allowed(`(cd ${home}/work); ${build}`); // the subshell cd does not leak
  denied(`cd ${home}/work && ${build}`, 'xcodebuild-derived-data');
});

test('fix 5: functions.exec keeps each nested exec_command call\'s own workdir', () => {
  const call = (code, cwd = '/Volumes/D/claude/app') => heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code }, cwd, platform: 'darwin' }, seam(cwd));
  const rule = (decision) => /\[([a-z-]+)\]/.exec(decision.hookSpecificOutput.permissionDecisionReason)[1];
  const bad = call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:"/Users/adrian"})');
  assert.equal(rule(bad), 'xcodebuild-derived-data'); // resolves against /Users/adrian, not the outer cwd
  assert.equal(call('await tools.exec_command({workdir:"/Users/adrian", cmd:"xcodebuild -scheme App build -derivedDataPath dd"})').hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:"/Volumes/D/claude/app"})', home), null);
  assert.equal(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:"app"})', '/Volumes/D/claude'), null);
  assert.equal(rule(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:"../../Users/adrian"})', '/Volumes/D/claude')), 'xcodebuild-derived-data');
  assert.equal(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd"})'), null); // no workdir: the outer cwd
  assert.equal(rule(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd"})', home)), 'xcodebuild-derived-data');
  const two = call('await tools.exec_command({cmd:"ls", workdir:"/Volumes/D/claude/app"}); await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd", workdir:"/Users/adrian"});');
  assert.equal(rule(two), 'xcodebuild-derived-data');
  assert.equal(call('await tools.exec_command({cmd:"echo a", workdir:"/Users/adrian"}); await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd", workdir:"/Volumes/D/claude/app"});', home), null);
  // Values the guard cannot read are refused rather than skipped.
  for (const code of [
    'await tools.exec_command({cmd: command})',
    'await tools.exec_command({cmd: "xcodebuild -scheme App build -derivedDataPath " + dir})',
    'await tools.exec_command({cmd: `xcodebuild -scheme App build -derivedDataPath ${dir}`})',
    'await tools.exec_command({cmd})',
    'await tools.exec_command({cmd: "xcodebuild -list", workdir})',
  ]) {
    const decision = call(code);
    assert.ok(decision, code);
    assert.equal(rule(decision), 'undeterminable-destination', code);
  }
  assert.equal(rule(call('await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:dir})')), 'undeterminable-destination');
  assert.equal(call('await tools.exec_command({cmd:"xcodebuild -list"})'), null);
  // Plain Codex exec_command resolves a relative workdir against the event cwd.
  assert.equal(heavyWriteHookDecision({ tool_name: 'exec_command', tool_input: { cmd: 'xcodebuild -scheme App build -derivedDataPath dd', workdir: 'app' }, cwd: '/Volumes/D/claude', platform: 'darwin' }, seam()), null);
  assert.equal(heavyWriteHookDecision({ tool_name: 'exec_command', tool_input: { cmd: 'xcodebuild -scheme App build -derivedDataPath dd', workdir: '/Users/adrian' }, cwd: '/Volumes/D/claude/app', platform: 'darwin' }, seam()).hookSpecificOutput.permissionDecision, 'deny');
});

test('fix 6: wrapper options are parsed with their real arity, or the command is refused', () => {
  denied(`sudo --user adrian xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`sudo --user=adrian xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`sudo -u adrian xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`sudo -uadrian xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`sudo -E -H --group staff -u adrian xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`sudo --user adrian --preserve-env=PATH xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`sudo --user adrian cp x ${cache}/y`, 'internal-heavy-path');
  denied(`env --unset FOO xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`env --unset=FOO FOO2=1 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`env -u FOO -i PATH=/usr/bin xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`env -uFOO xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`nice --adjustment=5 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`nice --adjustment 5 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`nice -n5 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`nice -10 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`time -p xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`timeout --signal=KILL --kill-after 5 600 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`caffeinate -dimsu -t 100 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`arch -arm64 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`arch -arch arm64 xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`xcrun --sdk=iphoneos xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`xcrun --sdk iphoneos xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`xcrun -sdk iphoneos --toolchain XcodeDefault -v xcodebuild -scheme App build`, 'xcodebuild-derived-data');
  denied(`xcrun --toolchain=XcodeDefault simctl create A ${device}`, 'simctl-no-simulators');
  denied(`xargs -I{} xcodebuild -scheme {} build`, 'undeterminable-destination'); // stdin controls the final argv of a tracked command
  denied(`nohup time nice -n 5 sudo -u adrian env FOO=1 xcrun simctl create A ${device}`, 'simctl-no-simulators');
  denied(`time -o ${cache}/t.txt true`, 'internal-heavy-path');
  allowed(`sudo --user adrian xcodebuild -scheme App build -derivedDataPath ${dd}`);
  allowed(`env --unset FOO -C ${ext} xcodebuild -scheme App build -derivedDataPath ${dd}`);
  // Options the guard does not model cannot be skipped over: the program name would be a guess.
  denied(`sudo --frobnicate xcodebuild -scheme App build`, 'undeterminable-destination');
  denied(`sudo -s 'xcodebuild -scheme App build'`, 'undeterminable-destination');
  denied(`sudo --login xcodebuild -scheme App build`, 'undeterminable-destination');
  denied(`env -S "xcodebuild build" `, 'undeterminable-destination');
  denied(`nice --weird xcodebuild -scheme App build`, 'undeterminable-destination');
  denied(`xcrun --weird xcodebuild -scheme App build`, 'undeterminable-destination');
  denied(`time -Z xcodebuild -scheme App build`, 'undeterminable-destination');
  denied(`sudo -u adrian $TOOL build`, 'undeterminable-destination');
  allowed('command -v xcodebuild');
  allowed('xcrun -f xcodebuild');
  allowed('xcrun --show-sdk-path');
});

test('fix 7: tar -C is a read directory when creating and the write destination when extracting', () => {
  allowed(`tar -cf ${ext}/cache.tar -C "$HOME/Library/Caches/cutright" .`);
  allowed(`tar -cf /Volumes/D/scratch/cache.tar -C "$HOME/Library/Caches/cutright" .`);
  allowed(`tar --create --file=${ext}/c.tar --directory="$HOME/Library/Caches/cutright" .`);
  allowed(`tar czf ${ext}/c.tgz -C ~/Library/Caches/cutright .`); // old style, no dash
  allowed(`tar -czf ${ext}/c.tgz -C ${cache} a b`);
  allowed(`tar -tf ${cache}/c.tar`);
  allowed(`tar -xf a.tar`); // cwd is /Volumes/D/claude/app
  allowed(`tar -xf ${cache}/a.tar -C ${ext}/x`); // the archive is read from the internal cache; the destination is external
  denied(`tar -xf a.tar -C ~/Library/Caches/cutright`, 'internal-heavy-path');
  denied(`tar xzf a.tgz -C ~/Library/Caches/cutright`, 'internal-heavy-path');
  denied(`tar --extract --file=a.tar --directory=${cache}`, 'internal-heavy-path');
  denied(`tar -xf a.tar -C ${ext} -C ${cache}`, 'internal-heavy-path'); // every -C of an extract is a destination
  denied(`tar -xf a.tar -C sub`, 'internal-heavy-path', cache); // relative -C resolved against the cwd
  denied(`tar -xf a.tar`, 'internal-heavy-path', cache); // extraction into the working directory
  denied(`tar -cf ${cache}/a.tar -C ${ext} .`, 'internal-heavy-path'); // the archive is the write destination
  denied(`tar --create --file ${cache}/a.tar .`, 'internal-heavy-path');
  denied(`tar -rf ${cache}/a.tar x`, 'internal-heavy-path'); // append writes the archive
  denied(`tar -cf a.tar -C ${ext} .`, 'undeterminable-destination');
  denied(`tar -C ${cache} .`, 'undeterminable-destination'); // no mode
  denied(`tar -cxf a.tar .`, 'undeterminable-destination'); // two modes
  denied(`tar -cf a.tar -Q`, 'undeterminable-destination'); // unknown option
  denied(`tar --frobnicate -cf a.tar`, 'undeterminable-destination');
  denied(`tar -xf a.tar`, 'undeterminable-destination', null); // extracts into an unknown directory
  denied(`tar -xf a.tar -C "$DEST"`, 'undeterminable-destination');
});

// ------------------------------------------------------------------------------------------------------------------
// #800 round 2: five more false allows. General rule: an unknown expression or option is never accepted as a
// known-safe prefix; it is refused. Counterexamples are data only.

test('round 2 fix 1: every writer option has its real arity; an unknown option is refused', () => {
  denied('rsync -az ./src/ /Volumes/E/backup/', 'non-agent-volume'); // -z is a flag, it must not swallow ./src/
  denied('rsync -avz ./src/ /Volumes/E/backup/', 'non-agent-volume');
  denied('rsync -azh --progress ./src/ /Volumes/E/backup/', 'non-agent-volume');
  denied('rsync -a -z -v ./src/ /Volumes/E/backup/', 'non-agent-volume');
  denied('rsync -az ./src/ /Volumes/E/backup/ -e ssh', 'non-agent-volume'); // -e takes a value wherever it sits
  denied(`rsync -az ./src/ ${cache}/`, 'internal-heavy-path');
  denied('rsync -ax --delete ./src/ /Volumes/E/backup/', 'non-agent-volume');
  denied('cp -X a /Volumes/E/y', 'non-agent-volume'); // -X is a flag
  denied('cp -pRv a /Volumes/E/y', 'non-agent-volume');
  denied('mv -fv a /Volumes/E/y', 'non-agent-volume');
  denied('scp -C -P 22 a /Volumes/E/y', 'non-agent-volume');
  denied('ditto -V a /Volumes/E/y', 'non-agent-volume');
  denied('install -m 644 -b a /Volumes/E/y', 'non-agent-volume');
  denied('ln -sfn a /Volumes/E/y', 'non-agent-volume');
  denied('mkdir -pv /Volumes/E/y', 'non-agent-volume');
  denied('touch -c -t 202001010000 /Volumes/E/y', 'non-agent-volume');
  denied('tee -a /Volumes/E/y', 'non-agent-volume');
  denied('curl -sSf --retry 3 -o /Volumes/E/y https://example.com/y', 'non-agent-volume');
  denied('curl --compressed -L -o /Volumes/E/y https://example.com/y', 'non-agent-volume');
  allowed(`rsync -az ./src/ ${ext}/backup/`);
  allowed('rsync -az -e ssh ./src/ host:/srv/backup/');
  allowed('rsync -avz --exclude node_modules ./src/ /Volumes/D/scratch/backup/');
  allowed('scp -C -P 22 a host:/srv/');
  // Options the tables do not know are refused instead of guessed.
  denied('rsync -Q ./src/ /Volumes/D/scratch/x/', 'undeterminable-destination');
  denied('rsync --frobnicate ./src/ /Volumes/E/backup/', 'undeterminable-destination');
  denied('cp -@ a /Volumes/D/scratch/y', 'undeterminable-destination');
  denied('mkdir -Z /Volumes/D/scratch/y', 'undeterminable-destination');
  denied('curl --some-new-flag -o /Volumes/D/scratch/y https://example.com/y', 'undeterminable-destination');
  denied('unzip -Z a.zip', 'undeterminable-destination');
  denied('wget --such-thing https://example.com/y', 'undeterminable-destination');
  denied('curl -sS -o /Volumes/D/scratch/y -Q', 'undeterminable-destination'); // -Q needs a value that is missing
  denied('cp $FLAGS a /Volumes/D/scratch/y', 'undeterminable-destination'); // an unevaluated word could be "-t /Users/..."
  denied('cp a $X/foo', 'undeterminable-destination'); // a word that STARTS with an expansion could be an option
  denied('mkdir "$D"', 'undeterminable-destination');
  denied('rsync -az $SRC /Volumes/D/scratch/x/', 'undeterminable-destination');
  denied('tar -cf /Volumes/D/scratch/a.tar $OPTS .', 'undeterminable-destination');
  denied('rsync --frobnicate=1 ./src/ /Volumes/E/backup/', 'undeterminable-destination'); // an unknown --name=value is refused too
  denied('rsync --no-motd --no-perms ./src/ /Volumes/E/backup/', 'non-agent-volume');
});

test('round 2 fix 2: an assignment that may not run never updates tracked variables', () => {
  const use = 'xcodebuild -scheme App build -derivedDataPath "$DD"';
  // The Oracle counterexample: the second assignment is skipped when `false` fails the && chain.
  denied(`DD=/Users/adrian/dd; false && DD=${dd}; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; false && DD=/Users/adrian/dd; ${use}`, 'undeterminable-destination'); // and the reverse
  denied(`DD=${dd}; true || DD=/Users/adrian/dd; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; true && DD=/Users/adrian/dd; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; if true; then DD=/Users/adrian/dd; fi; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; if false; then DD=${dd}; else DD=/Users/adrian/dd; fi; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; while false; do DD=/Users/adrian/dd; done; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; for x in a b; do DD=/Users/adrian/dd; done; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; case x in x) DD=/Users/adrian/dd;; esac; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; f() { DD=/Users/adrian/dd; }; f; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; DD=/Users/adrian/dd & ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; DD=/Users/adrian/dd | cat; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; export DD=/Users/adrian/dd || true; ${use}`, 'xcodebuild-derived-data'); // this export certainly runs: the later value wins
  denied(`DD=${dd}; false && export DD=/Users/adrian/dd; ${use}`, 'undeterminable-destination');
  // Builtins that change variables in ways the guard does not follow forget what it knew.
  denied(`DD=${dd}; eval 'DD=/Users/adrian/dd'; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; . ./env.sh; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; source ./env.sh; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; read DD; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; unset DD; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; mapfile DD < list.txt; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; for DD in /Users/adrian/dd; do :; done; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; printf -v DD '%s' /Users/adrian/dd; ${use}`, 'undeterminable-destination');
  denied(`DD=${dd}; let DD=1; ${use}`, 'undeterminable-destination');
  // An assignment that certainly runs still counts.
  allowed(`DD=/Users/adrian/dd; DD=${dd}; ${use}`);
  allowed(`DD=/Users/adrian/dd\nDD=${dd}\n${use}`);
  allowed(`export DD=${dd}; ${use}`);
  allowed(`DD=/Users/adrian/dd; export DD=${dd}; ${use}`);
  denied(`DD=${dd}; DD=/Users/adrian/dd; ${use}`, 'xcodebuild-derived-data');
  allowed(`(DD=/Users/adrian/dd); DD=${dd}; ${use}`); // the subshell assignment does not leak
  allowed(`DD=${dd}; (DD=/Users/adrian/dd; true); ${use}`); // the subshell assignment does not leak
});

test('round 2 fix 3: cwd and destinations resolve symlinks of each existing prefix BEFORE ".."', () => {
  // to-internal is a symlink to /Users/adrian/work, so to-internal/.. is /Users/adrian, not /Volumes/D/scratch.
  denied('env -C /Volumes/D/scratch/to-internal/.. xcodebuild -scheme App build -derivedDataPath dd', 'xcodebuild-derived-data');
  denied('env -C /Volumes/D/scratch/to-internal/../.. xcodebuild -scheme App build -derivedDataPath dd', 'xcodebuild-derived-data');
  denied('env --chdir=/Volumes/D/scratch/to-internal/.. xcodebuild -scheme App build -derivedDataPath dd', 'xcodebuild-derived-data');
  denied('sudo --chdir /Volumes/D/scratch/to-internal/.. xcodebuild -scheme App build -derivedDataPath dd', 'xcodebuild-derived-data');
  denied('sudo -D /Volumes/D/scratch/to-internal/.. xcodebuild -scheme App build -derivedDataPath dd', 'xcodebuild-derived-data');
  allowed('env -C /Volumes/D/scratch/../claude/app xcodebuild -scheme App build -derivedDataPath dd'); // no symlink: same either way
  allowed(`env -C ${ext} xcodebuild -scheme App build -derivedDataPath dd`);
  // `cd` is logical in most shells and physical with -P; when they disagree the directory is unknown.
  denied('cd /Volumes/D/scratch/to-internal/.. && xcodebuild -scheme App build -derivedDataPath dd', 'undeterminable-destination');
  denied('cd /Volumes/D/scratch/to-internal/.. && touch note', 'undeterminable-destination');
  // Relative destinations keep their ".." for the physical step.
  denied('cp x to-internal/../Library/Caches/cutright/y', 'internal-heavy-path', '/Volumes/D/scratch');
  denied('cd /Volumes/D/scratch && touch to-internal/../Library/Caches/cutright/y', 'internal-heavy-path');
  denied('echo x > to-internal/../Library/Caches/cutright/y', 'internal-heavy-path', '/Volumes/D/scratch');
  denied('tar -xf a.tar -C /Volumes/D/scratch/to-internal/../Library/Caches/cutright', 'internal-heavy-path');
  denied('tar -xf a.tar -C to-internal/../Library/Caches/cutright', 'internal-heavy-path', '/Volumes/D/scratch');
  denied('xcodebuild -scheme App build -derivedDataPath to-internal/../dd', 'xcodebuild-derived-data', '/Volumes/D/scratch'); // ".." is refused outright for agent-volume checks
  // Tool-supplied workdirs are chdir(2) arguments too.
  const exec = (workdir, cwd = home) => heavyWriteHookDecision({ tool_name: 'exec_command', tool_input: { cmd: 'xcodebuild -scheme App build -derivedDataPath dd', workdir }, cwd, platform: 'darwin' }, seam(cwd));
  assert.equal(exec('/Volumes/D/scratch/to-internal/..').hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(exec('to-internal/..', '/Volumes/D/scratch').hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(exec('/Volumes/D/claude/app'), null);
  assert.equal(exec('/Volumes/D/scratch/../claude/app'), null);
  const composed = (workdir) => heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code: `await tools.exec_command({cmd:"xcodebuild -scheme App build -derivedDataPath dd",workdir:"${workdir}"})` }, cwd: home, platform: 'darwin' }, seam(home));
  assert.equal(composed('/Volumes/D/scratch/to-internal/..').hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(composed('/Volumes/D/claude/app'), null);
});

test('round 2 fix 4: composed cmd and workdir must each be ONE complete string literal', () => {
  const call = (code, cwd = '/Volumes/D/claude/app') => heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code }, cwd, platform: 'darwin' }, seam(cwd));
  const rule = (decision) => /\[([a-z-]+)\]/.exec(decision.hookSpecificOutput.permissionDecisionReason)[1];
  const refused = [
    String.raw`await tools.exec_command({cmd:"echo safe " + "&& xcodebuild build"})`,
    String.raw`await tools.exec_command({cmd:"touch marker",workdir:"/Volumes/D" + "/../E"})`,
    String.raw`await tools.exec_command({cmd:"echo " + x})`,
    String.raw`await tools.exec_command({cmd:("xcodebuild -list")})`,
    String.raw`await tools.exec_command({cmd:"xcodebuild -list".trim()})`,
    String.raw`await tools.exec_command({cmd:["xcodebuild -list"][0]})`,
    String.raw`await tools.exec_command({cmd:"xcodebuild -list" ?? "ls"})`,
    String.raw`await tools.exec_command({cmd:"a" "b"})`,
    String.raw`await tools.exec_command({cmd:cond ? "ls" : "xcodebuild build"})`,
    String.raw`await tools.exec_command({cmd:"ls",workdir:"/Volumes/D" + x})`,
    String.raw`await tools.exec_command({cmd:"ls",workdir:base + "/x"})`,
    String.raw`await tools.exec_command({cmd:"ls",workdir:("/Volumes/D")})`,
    String.raw`await tools.exec_command({cmd:"ls",cmd:"xcodebuild build"})`,
    String.raw`await tools.exec_command({cmd:"ls",workdir:"/a",workdir:"/Volumes/E"})`,
    String.raw`await tools.exec_command({...base, cmd:"ls"})`,
    String.raw`await tools.exec_command({["c"+"md"]:"xcodebuild build"})`,
    String.raw`await tools.exec_command({cmd:"ls"}) // xcodebuild`,
    String.raw`await tools.exec_command({cmd:"ls", workdir:/x/.source})`,
    String.raw`await tools.exec_command({cmd(){ return "xcodebuild build" }})`,
    String.raw`await tools.exec_command({cmd:` + '`xcodebuild ${x}`' + String.raw`})`,
  ];
  for (const code of refused) {
    const decision = call(code);
    assert.ok(decision, code);
    assert.equal(rule(decision), 'undeterminable-destination', code);
  }
  // A single complete literal (any quote style) still works and is screened.
  assert.equal(rule(call(String.raw`await tools.exec_command({cmd:"xcodebuild -scheme App build"})`)), 'xcodebuild-derived-data');
  assert.equal(rule(call(String.raw`await tools.exec_command({cmd:'xcodebuild -scheme App build'})`)), 'xcodebuild-derived-data');
  assert.equal(rule(call('await tools.exec_command({cmd:`xcodebuild -scheme App build`})')), 'xcodebuild-derived-data');
  assert.equal(rule(call(String.raw`await tools.exec_command({"cmd":"xcodebuild -scheme App build"})`)), 'xcodebuild-derived-data');
  assert.equal(rule(call(String.raw`await tools.exec_command({command:"xcodebuild -scheme App build"})`)), 'xcodebuild-derived-data');
  assert.equal(call(String.raw`await tools.exec_command({cmd:"xcodebuild -list",workdir:"/Volumes/D/claude/app"})`), null);
});

test('round 2 fix 5: composed string literals are decoded exactly as JavaScript does, or refused', () => {
  const call = (code, cwd = '/Volumes/D/claude/app') => heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code }, cwd, platform: 'darwin' }, seam(cwd));
  const rule = (decision) => /\[([a-z-]+)\]/.exec(decision.hookSpecificOutput.permissionDecisionReason)[1];
  const screened = [
    String.raw`await tools.exec_command({cmd:"\x78codebuild -scheme App build"})`, // \xHH
    String.raw`await tools.exec_command({cmd:"xcodebuild -scheme App build"})`, // \uHHHH
    String.raw`await tools.exec_command({cmd:"\u{78}codebuild -scheme App build"})`, // \u{...}
    String.raw`await tools.exec_command({cmd:"\u{000078}codebuild -scheme App build"})`,
    String.raw`await tools.exec_command({cmd:'xcodebuild\x20-scheme App\tbuild'})`,
    String.raw`await tools.exec_command({cmd:"xcode\
build -scheme App build"})`, // line continuation
    String.raw`await tools.exec_command({cmd:"x\codebuild -scheme App build"})`, // identity escape \c
    String.raw`await tools.exec_command({cmd:"xcodebuild -scheme App build\0"})`, // \0 before a non-digit
    String.raw`await tools.exec_command({cmd:"xcodebuild \"-scheme\" App build"})`,
    'await tools.exec_command({cmd:`\\x78codebuild -scheme App build`})',
    String.raw`await tools.exec_command({"\x63md":"xcodebuild -scheme App build"})`, // an escaped KEY is decoded too
  ];
  for (const code of screened) {
    const decision = call(code);
    assert.ok(decision, code);
    assert.equal(rule(decision), 'xcodebuild-derived-data', code);
  }
  assert.equal(rule(call(String.raw`await tools.exec_command({cmd:"touch note",workdir:"\x2fUsers/adrian/Library/Caches/cutright"})`)), 'internal-heavy-path'); // escaped workdir
  assert.equal(rule(call(String.raw`await tools.exec_command({cmd:"touch note",workdir:"\u{2f}Volumes\x2fE"})`)), 'non-agent-volume');
  assert.equal(call(String.raw`await tools.exec_command({cmd:"touch note",workdir:"\x2fVolumes\x2fD/claude/app"})`), null);
  assert.equal(call(String.raw`await tools.exec_command({cmd:"\x78codebuild -list"})`), null); // decoded to a read-only query
  // Escapes the guard does not decode with certainty refuse the whole code.
  const undecoded = [
    String.raw`await tools.exec_command({cmd:"\170codebuild -scheme App build"})`, // legacy octal
    String.raw`await tools.exec_command({cmd:"xcodebuild \1"})`,
    String.raw`await tools.exec_command({cmd:"\00"})`,
    String.raw`await tools.exec_command({cmd:"\xZZcodebuild"})`,
    String.raw`await tools.exec_command({cmd:"\x7"})`,
    String.raw`await tools.exec_command({cmd:"\u12"})`,
    String.raw`await tools.exec_command({cmd:"\u{110000}"})`,
    String.raw`await tools.exec_command({cmd:"\u{}"})`,
    String.raw`await tools.exec_command({cmd:"\u{78"})`,
    String.raw`await tools.exec_command({cmd:"unterminated})`,
    'await tools.exec_command({cmd:"raw\nnewline"})',
    'await tools.exec_command({cmd:"ls", note:"\\xZZ"})', // an undecodable literal anywhere refuses the code
  ];
  for (const code of undecoded) {
    const decision = call(code);
    assert.ok(decision, code);
    assert.equal(rule(decision), 'undeterminable-destination', code);
  }
});

// ------------------------------------------------------------------------------------------------------------------
// #800 round 3 (Fable verdict 800-r3-agreed.md + Astra packet c4a2002b "R3 corrections"). Data only; nothing is executed.

test('round 3 G1: every unresolved destination is refused, including safe-looking prefixes', () => {
  denied('cp x /Volumes/D/scratch/$NAME', 'undeterminable-destination');
  denied('touch /Volumes/D/${REL}', 'undeterminable-destination'); // Astra: traversal through the unresolved tail
  denied('touch -- "$DEST"', 'undeterminable-destination');
  denied('tee /Volumes/D/scratch/$X', 'undeterminable-destination');
  denied('mkdir -p "/Volumes/D/scratch/$(date)"', 'undeterminable-destination');
  denied('echo x > /Volumes/D/scratch/$NAME', 'undeterminable-destination');
  denied('echo x >> "$LOG"', 'undeterminable-destination');
  denied('tar -cf /Volumes/D/scratch/$X.tar -C /Volumes/D/scratch .', 'undeterminable-destination');
  denied('cp x ~/Library/Caches/cutright/$X', 'internal-heavy-path'); // a protected head is denied with its own rule first
  denied('cp x /Volumes/E/$X', 'non-agent-volume');
  denied('echo x > /Volumes/E/$X', 'non-agent-volume');
  allowed('cp x /Volumes/D/scratch/y');
  allowed('$EDITOR /Volumes/D/scratch/x'); // an unevaluated program name is not itself a write destination
  denied('$EDITOR ~/Library/Caches/cutright/x', 'internal-heavy-path');
});

test('round 3 G2 + Astra mutation row: composed calls accept only direct exec_command calls with inline literals', () => {
  const call = (code, cwd = '/Volumes/D/claude/app') => heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code }, cwd, platform: 'darwin' }, seam(cwd));
  const rule = (decision) => /\[([a-z-]+)\]/.exec(decision.hookSpecificOutput.permissionDecisionReason)[1];
  const refused = [
    String.raw`const p={cmd:'touch /Volumes/D/x'}; p.cmd='touch /Volumes/E/x'; await tools.exec_command(p)`, // the Astra case
    String.raw`const o = {cmd:"xcodebuild -scheme App build"}; await tools.exec_command(o)`,
    String.raw`const o = {}; o["cmd"] = "xcodebuild build"; await tools.exec_command(o)`,
    String.raw`const o = {}; o.workdir = "/Volumes/E"; await tools.exec_command({cmd:"touch x"})`,
    String.raw`await tools.exec_command(JSON.parse(x))`,
    String.raw`await tools.exec_command(opts)`,
    String.raw`const f = tools.exec_command; await f({cmd:"xcodebuild build"})`,
    String.raw`const o = {}; Object.assign(o, {cmd:"x"}); await tools.exec_command(o)`,
    String.raw`await tools.exec_command({cmd:"ls", env:{cmd:"x"}})`,
    String.raw`const dir = "/Volumes/D"; await tools.exec_command({cmd:"ls", workdir:dir})`,
    String.raw`const args = {cmd:"ls"}; await tools.exec_command({...args})`,
    String.raw`await tools.exec_command()`,
    String.raw`await tools.exec_command("xcodebuild build")`,
  ];
  for (const code of refused) { const decision = call(code); assert.ok(decision, code); assert.equal(rule(decision), 'undeterminable-destination', code); }
  assert.equal(call(String.raw`await tools.exec_command({cmd:"xcodebuild -list"}, extra)`), null); // a direct inline literal call
  assert.equal(rule(call(String.raw`await tools.exec_command({cmd:"touch /Volumes/E/x"})`)), 'non-agent-volume');
});

test('round 3 Astra row 1: tar -h is --dereference, never help; index-file and listed-incremental outputs are always checked', () => {
  denied('tar -c -h -f /Volumes/E/a.tar /Volumes/D/src', 'non-agent-volume'); // the Astra case
  denied('tar -ch -f /Volumes/E/a.tar x', 'non-agent-volume');
  denied('tar --dereference -cf /Volumes/E/a.tar x', 'non-agent-volume');
  denied('tar -xhf a.tar -C /Volumes/E/x', 'non-agent-volume');
  denied('tar -h', 'undeterminable-destination'); // no mode
  allowed('tar --help');
  allowed('tar -?');
  allowed('tar --version');
  denied('tar -tf a.tar --index-file=/Volumes/E/idx', 'non-agent-volume'); // checked before the read-only (-t) return
  denied('tar --index-file /Volumes/E/idx -tf a.tar', 'non-agent-volume');
  denied(`tar -cf ${ext}/a.tar --listed-incremental=/Volumes/E/snar x`, 'non-agent-volume');
  denied(`tar -cf ${ext}/a.tar --listed-incremental ${cache}/snar x`, 'internal-heavy-path');
  denied('tar -xf a.tar --index-file=/Volumes/E/idx -C /Volumes/D/scratch/x', 'non-agent-volume');
  denied('tar --help --index-file=/Volumes/E/idx', 'undeterminable-destination'); // a help word never skips an output option
  denied('tar -g snap -cf /Volumes/D/scratch/a.tar x', 'undeterminable-destination'); // -g (GNU listed-incremental) is not modelled: refused
  allowed(`tar -tf a.tar --index-file=${ext}/idx`);
});

test('round 3 Astra row 2: install -d checks every operand; ln checks its implicit cwd destination', () => {
  denied('install -d /Volumes/E/newdir', 'non-agent-volume'); // the Astra case
  denied('install -d -m 755 /Volumes/D/scratch/a /Volumes/E/b', 'non-agent-volume'); // not just the last operand
  denied('install -d /Volumes/E/a /Volumes/D/scratch/b', 'non-agent-volume'); // not just the first
  denied(`install -d ${cache}/x`, 'internal-heavy-path');
  allowed('install -d /Volumes/D/scratch/a /Volumes/D/scratch/b');
  denied('install -c a /Volumes/E/y', 'non-agent-volume');
  denied('ln -s /tmp/model cwdE', 'non-agent-volume', '/Volumes/E'); // two operands: the last is the destination
  denied('ln -s /tmp/model', 'non-agent-volume', '/Volumes/E'); // the Astra case: one operand, link in the cwd
  denied('ln -s /tmp/model', 'internal-heavy-path', cache);
  denied('ln -s /tmp/model/', 'non-agent-volume', '/Volumes/E'); // trailing slash
  denied('ln -s /tmp/model', 'undeterminable-destination', null); // cwd unknown
  denied('cd /Volumes/E && ln -s /tmp/model', 'non-agent-volume');
  denied('ln -s $T', 'undeterminable-destination');
  allowed('ln -s /tmp/model'); // cwd /Volumes/D/claude/app
  allowed('ln -s /tmp/model /Volumes/D/scratch/link');
  denied('ln -s a /Volumes/E/link', 'non-agent-volume');
  denied('ln -t /Volumes/E/dir a b', 'non-agent-volume'); // -t DIR is the destination
  allowed('ln -t /Volumes/D/scratch a b');
  denied('ln -sf /tmp/model /Volumes/E/', 'non-agent-volume');
});

test('round 3 Astra row 3: curl implicit output and every file-writing option', () => {
  const url = 'https://example.com/x';
  denied(`curl --remote-name-all ${url}`, 'non-agent-volume', '/Volumes/E'); // the Astra case
  denied(`curl --remote-name-all ${url}`, 'internal-heavy-path', cache);
  denied(`curl --remote-name-all ${url}`, 'undeterminable-destination', null);
  denied(`curl -O ${url}`, 'non-agent-volume', '/Volumes/E');
  denied(`curl -J ${url}`, 'non-agent-volume', '/Volumes/E');
  denied(`curl -OJ ${url}`, 'non-agent-volume', '/Volumes/E');
  denied(`curl --remote-name-all ${url}`, 'download-outside-run-root'); // the default cwd is the workspace, which is not a run root
  allowed(`curl --remote-name-all ${url}`, ext);
  allowed(`curl -O --output-dir ${ext} ${url}`, '/Volumes/E'); // --output-dir redirects the implicit output
  denied(`curl -O --output-dir /Volumes/E/dir ${url}`, 'non-agent-volume');
  denied(`curl --etag-save /Volumes/E/tag ${url}`, 'non-agent-volume'); // the Astra case
  denied(`curl --etag-save=/Volumes/E/tag ${url}`, 'non-agent-volume');
  denied(`curl --hsts /Volumes/E/hsts ${url}`, 'non-agent-volume');
  denied(`curl --alt-svc /Volumes/E/alt ${url}`, 'non-agent-volume');
  denied(`curl -c /Volumes/E/jar ${url}`, 'non-agent-volume');
  denied(`curl --cookie-jar=/Volumes/E/jar ${url}`, 'non-agent-volume');
  denied(`curl -D /Volumes/E/h ${url}`, 'non-agent-volume');
  denied(`curl --dump-header /Volumes/E/h ${url}`, 'non-agent-volume');
  denied(`curl --trace /Volumes/E/t ${url}`, 'non-agent-volume');
  denied(`curl --trace-ascii /Volumes/E/t ${url}`, 'non-agent-volume');
  denied(`curl --stderr /Volumes/E/e ${url}`, 'non-agent-volume');
  denied(`curl --libcurl /Volumes/E/c.c ${url}`, 'non-agent-volume');
  denied(`curl -o /Volumes/E/o ${url}`, 'non-agent-volume');
  denied(`curl --etag-save ${cache}/tag ${url}`, 'internal-heavy-path');
  denied(`curl -K cfg ${url}`, 'undeterminable-destination'); // the config can set output
  denied(`curl --config /Volumes/D/scratch/curl.cfg ${url}`, 'undeterminable-destination');
  denied(`curl -w '%output{/Volumes/E/x}%{http_code}' -o /dev/null ${url}`, 'undeterminable-destination');
  denied(`curl --write-out "$FMT" -o /dev/null ${url}`, 'undeterminable-destination');
  allowed(`curl -w '%{http_code}' -o /dev/null ${url}`);
  allowed(`curl --etag-save ${ext}/tag --hsts ${ext}/hsts --alt-svc ${ext}/alt -c ${ext}/jar -D ${ext}/h ${url}`);
  allowed(`curl --create-dirs -o ${ext}/a/b ${url}`); // --create-dirs changes nothing about the destination
  denied(`curl --some-new-option=1 -o ${ext}/y ${url}`, 'undeterminable-destination'); // unknown --name=value is refused
});

test('round 3 Astra row 4: rsync batch files, log, temp and relative backup/partial directories', () => {
  denied('rsync -a --write-batch=/Volumes/E/batch ./src/ /Volumes/D/dst', 'non-agent-volume'); // the Astra case
  denied('rsync -a --write-batch /Volumes/E/batch ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a --only-write-batch=/Volumes/E/batch ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied(`rsync -a --write-batch=${cache}/batch ./src/ /Volumes/D/dst`, 'internal-heavy-path');
  denied(`rsync -a --write-batch=${ext}/$X ./src/ /Volumes/D/dst`, 'undeterminable-destination');
  allowed(`rsync -a --write-batch=${ext}/batch ./src/ /Volumes/D/dst`); // F and F.sh both on the agent volume
  denied('rsync -a --log-file=/Volumes/E/log ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a --temp-dir=/Volumes/E/t ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a -T /Volumes/E/t ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a --backup-dir=/Volumes/E/bk ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a --partial-dir=/Volumes/E/p ./src/ /Volumes/D/dst', 'non-agent-volume');
  denied('rsync -a --backup-dir=bk ./src/ /Volumes/E/dst', 'non-agent-volume');
  allowed('rsync -a --backup-dir=bk --partial-dir=.p ./src/ /Volumes/D/dst', cache); // relative ones resolve against /Volumes/D/dst, not the cwd
  allowed('rsync -a --backup-dir=bk ./src/ host:/srv/dst/'); // receiver side is remote
  denied('rsync -a --backup-dir=bk ./src/ "$DEST"', 'undeterminable-destination');
  allowed('rsync -a --files-from=/Volumes/E/list --include-from=/Volumes/E/inc --exclude-from=/Volumes/E/exc ./src/ /Volumes/D/dst'); // reads
  denied('rsync -a --frobnicate=1 ./src/ /Volumes/D/dst', 'undeterminable-destination'); // unknown --name=value
  denied('rsync -a --no-frobnicate ./src/ /Volumes/D/dst', 'undeterminable-destination'); // --no-X only when X is known
  allowed('rsync -a --no-perms ./src/ /Volumes/D/dst');
});

test('round 3 Astra row 5 + R3: find -execdir/-okdir are refused; {} destinations are always unresolved; -delete and -fprint are destinations', () => {
  denied('find /Volumes/E/cache/file -execdir touch marker \\;', 'undeterminable-destination'); // the Astra case
  denied('find /Volumes/D/scratch -execdir touch /Volumes/D/scratch/m \\;', 'undeterminable-destination'); // refused even when absolute
  denied('find /Volumes/D/scratch -okdir touch marker \\;', 'undeterminable-destination');
  denied('find /Volumes/D/scratch -execdir true \\;', 'undeterminable-destination');
  denied('find -L /Volumes/D -exec touch {} \\;', 'undeterminable-destination'); // a child symlink can reach /Volumes/E
  denied('find /Volumes/D/scratch -exec touch {} \\;', 'undeterminable-destination'); // never resolved, even with agent roots
  denied('find /Volumes/D/scratch -exec cp x {}.bak \\;', 'undeterminable-destination');
  denied('find /Volumes/D/scratch -exec cp {} /Volumes/D/scratch/out/ \\;', 'undeterminable-destination');
  denied('find /Volumes/D/scratch -exec cp x /Volumes/E/y \\;', 'non-agent-volume');
  denied('find /Volumes/D/scratch -exec sh -c \'touch "$1"\' _ {} \\;', 'undeterminable-destination');
  allowed('find /Volumes/D/scratch -name "*.log" -exec grep -l error {} \\;'); // a command the guard does not track writes nothing it can see
  allowed('find /Volumes/D/scratch -exec touch /Volumes/D/scratch/marker \\;');
  denied('find /Volumes/E/cache -delete', 'non-agent-volume');
  denied(`find ${cache} -delete`, 'internal-heavy-path');
  denied('find $X -delete', 'undeterminable-destination');
  denied('find . -delete', 'non-agent-volume', '/Volumes/E'); // implicit start path is the cwd
  allowed('find /Volumes/D/scratch/old -delete');
  denied('find /Volumes/D/scratch -fprint /Volumes/E/f', 'non-agent-volume');
  denied('find /Volumes/D/scratch -fprint0 /Volumes/E/f', 'non-agent-volume');
  denied('find /Volumes/D/scratch -fprintf /Volumes/E/f %p', 'non-agent-volume');
  denied('find /Volumes/D/scratch -fls /Volumes/E/f', 'non-agent-volume');
  allowed(`find /Volumes/D/scratch -fprint ${ext}/f`);
});

test('round 3 Astra row 6 + R3: attached option values are sliced, expanded once, and keep their dynamic status', () => {
  const url = 'https://example.com/x';
  denied(`curl -o"$HOME/Library/Caches/cutright/$USER" ${url}`, 'internal-heavy-path'); // the Astra case
  denied(`curl --output="$HOME/Library/Caches/cutright/$USER" ${url}`, 'internal-heavy-path');
  denied(`curl -sSLo"$HOME/Library/Caches/cutright/$USER" ${url}`, 'internal-heavy-path');
  denied(`dd of="$HOME/Library/Caches/cutright/$USER"`, 'internal-heavy-path');
  denied(`dd of=/Volumes/D/scratch/$X`, 'undeterminable-destination');
  denied(`dd if=/dev/zero of=/Volumes/E/$X`, 'non-agent-volume');
  denied(`curl -o/Volumes/D/scratch/$X ${url}`, 'undeterminable-destination');
  denied(`curl --output=/Volumes/D/${'${REL}'} ${url}`, 'undeterminable-destination');
  denied(`curl -o$X ${url}`, 'undeterminable-destination');
  denied(`curl -o"$X" ${url}`, 'undeterminable-destination');
  denied(`curl --output=/Volumes/E/$X ${url}`, 'non-agent-volume');
  denied(`unzip a.zip -d"$HOME/Library/Developer/CoreSimulator/Devices/$UDID"`, 'internal-heavy-path');
  denied(`wget -P"/Volumes/D/scratch/$X" ${url}`, 'undeterminable-destination');
  // No second expansion: a literal "$" that is already in a parsed value is data, never re-expanded.
  allowed(`curl -o'$HOME/Library/Caches/cutright/x' ${url}`, ext); // single quotes: a relative file named "$HOME/..." inside the cwd (a run root)
  allowed(`curl -o"\\$HOME/Library/Caches/cutright/x" ${url}`, ext); // an escaped dollar in double quotes is the same literal
  allowed(`curl --output='$X' ${url}`, ext);
  denied(`curl --output='$X' ${url}`, 'download-outside-run-root'); // the same literal relative file in the workspace cwd
  allowed(`dd of='$X'`);
  denied(`curl -o"\\$HOME/x" ${url}`, 'internal-heavy-path', cache); // the literal path is relative to the (protected) cwd
  // An unresolved expansion inside a short-option cluster could be more option letters.
  denied('cp -a$X a /Volumes/D/scratch/y', 'undeterminable-destination');
  denied('cp -a"$X" a /Volumes/D/scratch/y', 'undeterminable-destination');
  denied(`curl -sS$X -o ${ext}/y ${url}`, 'undeterminable-destination');
  denied(`tar -c$X -f ${ext}/a.tar .`, 'undeterminable-destination');
  denied('sed -n$X p f', 'undeterminable-destination');
});

test('round 3 self-audit: cp/mv backup suffix, tee -a, xargs-wrapped writers, sed -i, git clone and worktree', () => {
  // cp/mv --backup/-b with a suffix leaves the destination unchanged.
  denied('mv -S .bak a /Volumes/E/y', 'non-agent-volume');
  denied('mv a /Volumes/E/y -S .bak', 'non-agent-volume'); // -S takes its value wherever it sits
  denied('cp -b --suffix=.bak a /Volumes/E/y', 'non-agent-volume');
  denied('cp --backup=numbered a /Volumes/E/y', 'non-agent-volume');
  allowed('cp -b -S .bak a /Volumes/D/scratch/y');
  denied('tee -a /Volumes/E/y', 'non-agent-volume');
  denied('tee -a /Volumes/D/scratch/$X', 'undeterminable-destination');
  // xargs: refuse when stdin controls the final argv of a tracked command.
  for (const command of ['xargs cp x', 'xargs -I{} cp {} /Volumes/E/x', 'xargs -I{} cp x {}', 'xargs cp -t /Volumes/D/scratch', 'xargs touch', 'xargs mkdir -p /Volumes/D/scratch/x', "xargs sed -i '' s/a/b/", 'xargs git clone', 'xargs -0 tar -xf', 'xargs xcodebuild -scheme App build', 'xargs $CMD', 'xargs sh -c "cp x y"', 'xargs sudo cp x', 'xargs env cp x']) denied(command, 'undeterminable-destination');
  allowed('xargs -0 rm');
  allowed('xargs echo');
  allowed('xargs');
  allowed('xargs -n1 grep -l foo');
  // sed -i: BSD (-i '' / -i EXT) and GNU (-i, -iSUFFIX) forms; w/W scripts and unsupported forms are refused.
  denied("sed -i '' 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("sed -i .bak 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("sed -i.bak 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("sed -i '' -e 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("sed -i '' 's/a/b/' /Volumes/D/scratch/a /Volumes/E/b", 'non-agent-volume');
  denied(`sed -i '' 's/a/b/' ${cache}/f`, 'internal-heavy-path');
  denied("gsed -i 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("gsed -i -e 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("gsed -i.bak -e 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("gsed --in-place=.bak -e 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  denied("gsed --in-place 's/a/b/' /Volumes/E/f", 'non-agent-volume');
  allowed("sed -i '' 's/a/b/' /Volumes/D/scratch/f");
  allowed("sed -i.bak 's/a/b/' /Volumes/D/scratch/f");
  allowed("gsed -i 's/a/b/' /Volumes/D/scratch/f");
  allowed("sed -n 's/a/b/p' /Volumes/E/f"); // no -i: reading
  allowed("sed -e 's/www/x/' f"); // w inside a plain substitution is data
  denied("sed -i -e 's/a/b/' /Volumes/D/scratch/f", 'undeterminable-destination'); // BSD would take -e as the suffix
  denied("sed -i 's/a/b/' /Volumes/D/scratch/f", 'undeterminable-destination'); // BSD would take the script as the suffix
  denied("sed -i '' 's/a/b/'", 'undeterminable-destination'); // no file operand
  denied("sed -i '' 's/a/b/' \"$F\"", 'undeterminable-destination');
  denied("sed -i '' \"s/$A/$B/\" /Volumes/D/scratch/f", 'undeterminable-destination'); // a dynamic script with -i may hold a w command
  // A literal script is read: every w/W FILE and s///w FILE is a destination; e and s///e are refused; a script that is not
  // literal (any expansion) or comes from -f is refused with or without -i.
  denied("sed -n 'w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed 's/a/b/w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed 's/a/b/gw /Volumes/E/out' f", 'non-agent-volume');
  denied("sed -e 'W /Volumes/E/out' f", 'non-agent-volume');
  denied("sed -e 'p' -e 'w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed '1,3w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed '/x/w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed '$w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed '/x/I{w /Volumes/E/out\n}' f", 'non-agent-volume');
  denied("sed 's/a/b/;w /Volumes/E/out' f", 'non-agent-volume'); // the file name runs to the end of the line
  denied("sed 'y/abc/xyz/;w /Volumes/E/out' f", 'non-agent-volume');
  denied("sed '1d;a\\ntext\nw /Volumes/E/out' f", 'non-agent-volume');
  denied(`sed -n 'w ${cache}/out' f`, 'internal-heavy-path');
  denied("sed 's/a/b/w out' f", 'internal-heavy-path', cache); // a relative file name resolves against the cwd
  denied("sed '1,3w out' f", 'non-agent-volume', '/Volumes/E');
  denied("sed -n 'w out' f", 'undeterminable-destination', null);
  denied("sed -n 'w' f", 'undeterminable-destination'); // no file name
  denied("sed -i '' 's/a/b/w /Volumes/E/out' /Volumes/D/scratch/f", 'non-agent-volume'); // with -i too
  allowed(`sed -n 'w ${ext}/out' f`);
  allowed("sed -n 'w /dev/stdout' f");
  allowed("sed 's/a/b/;s/www/x/' f"); // a w inside a pattern or replacement is data
  allowed("sed -e 's/www/x/gp' -e '/wow/d' f");
  allowed("sed -n '1,3p;$p' f");
  allowed("sed 'y/abc/wxy/' f");
  allowed("sed '1d' f");
  allowed("sed 's/a/b/' f");
  allowed("sed '/x/{s/a/b/;p}' f");
  denied("gsed 's/a/b/e' f", 'undeterminable-destination'); // s///e executes commands
  denied("sed 's/a/b/ge' f", 'undeterminable-destination');
  denied("gsed 'e echo hi' f", 'undeterminable-destination'); // the e command
  denied("gsed '1e date' f", 'undeterminable-destination');
  denied("sed 'e' f", 'undeterminable-destination');
  denied("sed 's/a/b/z' f", 'undeterminable-destination'); // an unknown flag is not guessed
  denied("sed 's/a/b' f", 'undeterminable-destination'); // unterminated
  denied("sed 'Q;k' f", 'undeterminable-destination'); // an unknown command
  denied('sed "s/$A/b/" f', 'undeterminable-destination'); // a dynamic script, without -i
  denied('sed -n "${N}p" f', 'undeterminable-destination');
  denied('sed -n "$S" f', 'undeterminable-destination');
  denied('sed -e "s/a/b/" -e "$EXTRA" f', 'undeterminable-destination');
  denied('sed --expression="$S" f', 'undeterminable-destination');
  denied('sed -n "$(cat s.sed)" f', 'undeterminable-destination');
  denied('sed -e $(cat s.sed) f', 'undeterminable-destination');
  denied('sed -f s.sed f', 'undeterminable-destination'); // a script from a file, without -i
  denied('sed -n -f s.sed f', 'undeterminable-destination');
  denied('sed --file s.sed f', 'undeterminable-destination');
  denied('gsed -f s.sed f', 'undeterminable-destination');
  denied('gsed -nf s.sed f', 'undeterminable-destination');
  denied("sed -i '' -f s.sed /Volumes/D/scratch/f", 'undeterminable-destination');
  denied('sed -f script.sed f', 'undeterminable-destination');
  denied('sed --file=script.sed f', 'undeterminable-destination');
  denied('sed -Q f', 'undeterminable-destination');
  denied('sed --frobnicate=1 f', 'undeterminable-destination');
  // git clone / worktree add|move.
  const repo = 'https://example.com/y.git';
  denied(`git clone ${repo} /Volumes/E/y`, 'non-agent-volume');
  denied(`git clone ${repo}`, 'non-agent-volume', '/Volumes/E'); // default directory <cwd>/<repo name>
  denied(`git clone ${repo}`, 'internal-heavy-path', cache);
  denied(`git clone --bare ${repo}`, 'non-agent-volume', '/Volumes/E');
  denied(`git clone ${repo}`, 'undeterminable-destination', null);
  denied('git clone git@example.com:org/y /Volumes/E/y', 'non-agent-volume');
  denied(`git -C /Volumes/E clone ${repo}`, 'non-agent-volume'); // -C is the cwd
  denied(`git -C /Volumes/D/scratch/to-internal/../Library/Caches/cutright clone ${repo}`, 'internal-heavy-path'); // physical ".."
  denied(`git clone --separate-git-dir=/Volumes/E/g ${repo} /Volumes/D/scratch/y`, 'non-agent-volume');
  denied(`git clone ${repo} ~/Library/Caches/cutright/y`, 'internal-heavy-path');
  denied(`git clone ${repo}`, 'repo-create-outside-run-root'); // the default cwd is the workspace
  denied(`git clone ${repo} /Volumes/D/scratch/y`, 'repo-create-outside-run-root'); // no lease
  denied(`git -C /Volumes/D/scratch clone ${repo} sub`, 'repo-create-outside-run-root');
  denied(`git -C /Volumes/D/scratch -C ../claude clone ${repo}`, 'repo-create-outside-run-root'); // ordered -C: the second is relative to the first
  allowed(`git clone ${repo}`, ext);
  allowed(`git clone ${repo} ${ext}/y`);
  allowed(`git -C ${ext} clone ${repo} sub`);
  allowed(`git -C /Volumes/D/scratch -C run1 clone ${repo}`); // ordered -C: the second is relative to the first
  allowed(`git -C /Volumes/E clone ${repo} ${ext}/y`); // an absolute destination ignores the cwd
  denied(`git worktree add /Volumes/E/w HEAD`, 'non-agent-volume'); // the Astra case: HEAD is the commit-ish
  denied(`git worktree add -b br /Volumes/E/wt main`, 'non-agent-volume');
  denied(`git worktree add --detach /Volumes/E/wt`, 'non-agent-volume');
  denied(`git -C /Volumes/E worktree add wt HEAD`, 'non-agent-volume');
  denied(`git worktree move a /Volumes/E/b`, 'non-agent-volume');
  allowed(`git worktree add ${ext}/w /Volumes/E/not-a-destination`); // the trailing operand is never the destination
  denied(`git worktree add /Volumes/D/scratch/w /Volumes/E/not-a-destination`, 'repo-create-outside-run-root'); // no lease
  denied(`git worktree add ../wt`, 'repo-create-outside-run-root');
  allowed(`git worktree add wt`, ext);
  allowed('git worktree list');
  allowed('git status');
  for (const command of ['git clone', `git clone ${repo} a b`, 'git worktree add', 'git worktree add a b c', 'git worktree move a', 'git worktree add $X', 'git clone --frobnicate u d', 'git --frobnicate clone u d', 'git -C "$X" clone u d', 'git $SUB clone u d', 'git clone $URL', 'git clone --frobnicate=1 u d']) denied(command, 'undeterminable-destination');
});

test('Codex exec_command and composed functions.exec payloads are inspected', () => {
  const codex = heavyWriteHookDecision({ tool_name: 'exec_command', tool_input: { cmd: 'xcodebuild -scheme App build', workdir: '/Volumes/D/claude/app' }, platform: 'darwin' }, { fs, home });
  assert.equal(codex.hookSpecificOutput.permissionDecision, 'deny');
  const composed = heavyWriteHookDecision({ tool_name: 'functions.exec', tool_input: { code: 'await tools.exec_command({ cmd: "xcrun simctl create A t" })' }, cwd: home, platform: 'darwin' }, { fs, home });
  assert.equal(composed.hookSpecificOutput.permissionDecision, 'deny');
  const ok = heavyWriteHookDecision({ tool_name: 'exec_command', tool_input: { cmd: `xcodebuild -scheme App build -derivedDataPath ${dd}` }, cwd: home, platform: 'darwin' }, { fs, home });
  assert.equal(ok, null);
});

test('non-macOS hosts are not affected', () => {
  for (const platform of ['linux', 'win32', 'freebsd']) {
    for (const command of ['xcrun simctl create A t', 'xcodebuild -scheme App build', 'cp x ~/Library/Caches/cutright/y', 'cp x /Volumes/E/y']) {
      assert.equal(heavyWriteHookDecision({ tool_name: 'Bash', tool_input: { command }, cwd: home, platform }, seam(home)), null, `${platform}: ${command}`);
    }
    assert.equal(heavyWriteHookDecision({ tool_name: 'Write', tool_input: { file_path: `${cache}/x` }, cwd: home, platform }, seam(home)), null);
  }
  assert.equal(ownershipWriteDecision({ tool_name: 'Bash', tool_input: { command: 'xcodebuild -scheme App build' }, cwd: home, platform: 'linux' }).hookSpecificOutput?.permissionDecision, undefined);
});

test('the ownership write hook runs the guard first on macOS and still allows unrelated commands', () => {
  const input = (command) => ({ tool_name: 'Bash', tool_input: { command }, cwd: '/Volumes/D/claude/app', platform: 'darwin' });
  const options = { heavyWrite: seam() };
  const blocked = ownershipWriteDecision(input('xcrun simctl create A t'), options);
  assert.equal(blocked.hookSpecificOutput.permissionDecision, 'deny');
  assert.match(blocked.hookSpecificOutput.permissionDecisionReason, /rightkit-heavy-write/);
  assert.equal(ownershipWriteDecision(input(`xcodebuild -scheme App build -derivedDataPath ${dd}`), options).hookSpecificOutput?.permissionDecision, undefined);
  assert.equal(ownershipWriteDecision(input('ls -la'), options).decision, 'allow');
});

test('app owners come from the packaged repository registry', () => {
  const owners = loadAppOwners();
  for (const owner of ['cutright', 'heardright', 'mailright', 'swiftright', 'rightkit']) assert.ok(owners.includes(owner), owner);
});

test('the guard ships with the installed policy assets and loads from the installed copy', async () => {
  const root = mkdtempSync(join(tmpdir(), 'rk-heavy-guard-'));
  try {
    const installed = await installPolicyAssets({ root });
    const manifest = JSON.parse(readFileSync(join(installed.installRoot, 'manifest.json'), 'utf8'));
    assert.ok(manifest.files.some((entry) => entry.name === 'agent-heavy-write-guard.mjs'));
    const imported = await import(pathToFileURL(join(installed.installRoot, 'src/agent-heavy-write-guard.mjs')).href);
    assert.ok(imported.loadAppOwners().includes('cutright'));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('real symlink to an internal directory is refused, a real agent-volume path is accepted', { skip: process.platform === 'win32' ? 'POSIX symlinks and paths only' : false }, () => {
  const base = realpathSync(mkdtempSync(join(tmpdir(), 'rk-heavy-real-')));
  try {
    const volumes = join(base, 'Volumes'); const internal = join(base, 'home', 'internal');
    mkdirSync(join(volumes, 'D', 'scratch'), { recursive: true }); mkdirSync(internal, { recursive: true });
    mkdirSync(join(volumes, 'D-evil'), { recursive: true }); mkdirSync(join(volumes, 'E'), { recursive: true });
    symlinkSync(internal, join(volumes, 'D', 'link'), 'dir');
    symlinkSync(join(base, 'home', 'missing'), join(volumes, 'D', 'dangling'), 'dir');
    // st_dev stand-in: D and E are separate "external" devices; everything else shares the system device.
    const under = (target, dir) => target === dir || target.startsWith(`${dir}/`);
    const real = { ...realFs, device: (target) => (under(target, join(volumes, 'D')) ? 3 : under(target, join(volumes, 'E')) ? 4 : 1) };
    const options = { fs: real, home: join(base, 'home'), volumesRoot: volumes, systemRoots: [base, join(base, 'home')], cwd: base };
    const build = (dir) => heavyCommandViolation(`xcodebuild -scheme App build -derivedDataPath "${dir}"`, options);
    assert.equal(build(join(volumes, 'D', 'scratch', 'dd')), null);
    assert.equal(build(join(volumes, 'D', 'link', 'dd'))?.rule, 'xcodebuild-derived-data');
    assert.equal(build(join(volumes, 'D', 'dangling', 'dd'))?.rule, 'xcodebuild-derived-data');
    assert.equal(build(join(volumes, 'D-evil', 'dd'))?.rule, 'xcodebuild-derived-data');
    assert.equal(build(join(volumes, 'E', 'dd'))?.rule, 'xcodebuild-derived-data'); // a real mount that is not an agent volume
    assert.equal(build(`${join(volumes, 'D', 'scratch')}/../../../home/x`)?.rule, 'xcodebuild-derived-data');
    assert.equal(build(join(base, 'home', 'internal', 'dd'))?.rule, 'xcodebuild-derived-data');
    assert.equal(heavyCommandViolation(`cp x ${join(volumes, 'E', 'y')}`, options)?.rule, 'non-agent-volume');
    assert.equal(heavyCommandViolation(`cp x ${join(volumes, 'D', 'scratch', 'y')}`, options), null);
    assert.equal(heavyCommandViolation(`tar -cf ${join(volumes, 'D', 'scratch', 'a.tar')} -C ${join(base, 'home', 'internal')} .`, options), null);
  } finally { rmSync(base, { recursive: true, force: true }); }
});

// ---------------------------------------------------------------------------------------------------------------
// Round 4: git explicit model (git's own resolution through a bounded read-only seam), find -L -delete, forked find scope.
// Every command string is DATA ONLY: parsed, never executed. The git runner is a fake.
test('round 4 git (a): pure object readers are allowed anywhere and never spawn git', () => {
  for (const cwd of ['/Volumes/D/claude/app', '/Volumes/E', home, null]) {
    const gitRunner = fakeGit();
    for (const command of ['git log --oneline -5', 'git show HEAD', 'git log -p --stat', 'git rev-parse HEAD', 'git ls-files', 'git ls-tree -r HEAD', 'git cat-file -p HEAD',
      'git branch', 'git branch -a', "git branch --list 'x*'", 'git branch --contains HEAD', 'git tag', 'git tag -l', 'git tag --points-at HEAD', 'git remote -v', 'git remote show origin', 'git remote get-url origin',
      'git config --get user.name', 'git config user.name', 'git config --list', 'git worktree list', 'git bundle verify x.bundle', 'git blame x', 'git ls-files -o', 'git ls-files --cached', 'git ls-files -s', 'git reflog', 'git reflog show HEAD', 'git --no-pager log', 'git --version', 'git diff --no-index a b', 'git diff-tree -r HEAD']) {
      assert.equal(check(command, cwd, { gitRunner }), null, command);
    }
    assert.equal(gitRunner.calls.length, 0, 'read-only forms never spawn git');
  }
});

test('round 4 git (b): repo-local writers are admitted only on the resolution git itself reports, which must be on the agent volume', () => {
  const gitRunner = fakeGit();
  for (const command of ['git add -A', 'git commit -m "x"', 'git checkout -b feature', 'git switch main', 'git restore x', 'git reset --hard', 'git merge main', 'git rebase main', 'git stash', 'git stash pop',
    'git fetch origin', 'git pull origin main', 'git push origin main', 'git push -u origin main', 'git gc', 'git branch newb', 'git tag v1', 'git tag -a v1 -m x', 'git branch -d old',
    'git remote add up https://example.com/r.git', 'git config user.name x', 'git cherry-pick abc', 'git revert abc', 'git -C /Volumes/D/scratch add .']) {
    assert.equal(check(command, undefined, { gitRunner }), null, command);
  }
  const first = gitRunner.calls[0];
  assert.deepEqual(first.args, ['-c', 'core.fsmonitor=false', '-c', 'core.untrackedCache=false', 'rev-parse', '--path-format=absolute', '--show-toplevel', '--absolute-git-dir', '--git-common-dir']);
  assert.ok(first.timeout > 0 && first.timeout <= 5000);
  assert.equal(first.cwd, '/Volumes/D/claude/app');
  assert.equal(gitRunner.calls.at(-1).cwd, '/Volumes/D/scratch', '-C is honoured (and resolved physically)');
  // the repository directory itself must be on the agent volume, before git is even asked
  for (const [command, cwd] of [['git commit -m x', '/Volumes/E'], ['git add .', home], ['git -C /Volumes/E add .', undefined], ['git -C /Volumes/D/scratch/to-internal add .', undefined], ['git push origin main', cache]]) {
    const spy = fakeGit();
    denied(command, undefined, cwd, { gitRunner: spy });
    assert.equal(spy.calls.length, 0, `${command}: no spawn for a repository directory off the agent volume`);
  }
  denied('git commit -m x', 'undeterminable-destination', null);
  denied('git -C $X commit -m x', 'undeterminable-destination');
});

test('round 4 git closure: work tree, git dir and common dir as git resolves them must all be on the agent volume', () => {
  const here = '/Volumes/D/claude/app';
  const repos = (patch) => ({ gitRunner: fakeGit({ repos: { [here]: patch } }) });
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ top: '/Volumes/E/wt' })); // core.worktree
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ gitDir: '/Volumes/E/.git' })); // gitdir: file pointing at E
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ common: '/Volumes/E/common.git' })); // commondir on E
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ common: `${home}/work/common.git` }));
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ gitDir: '/Volumes/D/scratch/to-internal/g' })); // symlink out of D, resolved physically
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ gitDir: '/Volumes/D/scratch/../../E/g' })); // ".." through D onto E
  denied('git commit -m x', 'non-agent-volume', undefined, repos({ gitDir: '/Volumes/D-evil/g' }));
  denied('git commit -m x', 'undeterminable-destination', undefined, repos({ gitDir: '/Volumes/D/scratch/dangling/g' }));
  allowed('git commit -m x', undefined, repos({ top: '/Volumes/D/scratch/wt', gitDir: '/Volumes/D/claude/app/.git/worktrees/wt', common: '/Volumes/D/claude/app/.git' }));
  // failure, timeout, unparseable output
  for (const extra of [{ gitRunner: fakeGit({ status: 128 }) }, { gitRunner: fakeGit({ status: null }) }, { gitRunner: fakeGit({ throws: Object.assign(new Error('timed out'), { code: 'ETIMEDOUT' }) }) },
    { gitRunner: fakeGit({ throws: Object.assign(new Error('no git'), { code: 'ENOENT' }) }) }, { gitRunner: fakeGit({ out: 'nonsense\n' }) }, { gitRunner: fakeGit({ out: '/Volumes/D/a\n/Volumes/D/a/.git\n' }) },
    { gitRunner: fakeGit({ out: 'a\nb\nc\n' }) }, { gitRunner: fakeGit({ out: '' }) }, { gitRunner: fakeGit({ out: '/Volumes/D/a\n/Volumes/D/a/.git\n/Volumes/D/a/.git\n/Volumes/E\n' }) }]) {
    denied('git commit -m x', 'undeterminable-destination', undefined, extra);
  }
  // no time left: the budget is 5 s per hook call and git is not spawned once it is used up
  let clock = 0; const spy = fakeGit();
  assert.equal(check('git commit -m x', undefined, { gitRunner: spy, clock: () => (clock += 5000) }), null, 'one spawn fits the 5 s budget');
  clock = 0; const second = fakeGit();
  denied('git push origin main', 'undeterminable-destination', undefined, { gitRunner: second, clock: () => (clock += 5000) }); // rev-parse used the budget; remote -v is not spawned
  assert.equal(second.calls.length, 1);
  assert.ok(second.calls.every((call) => call.timeout <= 5000));
  // the default runner scrubs the environment (PATH and HOME only); the seam receives home and PATH
  const seen = fakeGit();
  check('git commit -m x', undefined, { gitRunner: seen, env: { PATH: '/usr/bin', GIT_PAGER: 'cat' } });
  assert.equal(seen.calls[0].path, '/usr/bin');
  assert.equal(seen.calls[0].home, home);
});

test('round 4 git env: any GIT_* outside the harmless list is refused for writers (process env, prefix, export, env)', () => {
  for (const name of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_KEY_0', 'GIT_CONFIG_VALUE_0',
    'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_SYSTEM', 'GIT_CONFIG', 'GIT_SSH_COMMAND', 'GIT_EXEC_PATH', 'GIT_NAMESPACE', 'GIT_PROXY_COMMAND', 'GIT_ASKPASS', 'GIT_EXTERNAL_DIFF']) {
    denied(`${name}=/Volumes/E/x git commit -m x`, 'undeterminable-destination');
    denied(`export ${name}=/Volumes/E/x; git add .`, 'undeterminable-destination');
    denied(`env ${name}=/Volumes/E/x git add .`, 'undeterminable-destination');
    denied('git add .', 'undeterminable-destination', undefined, { env: { [name]: '/Volumes/E/x' } });
  }
  denied('GIT_DIR=$X git commit -m x', 'undeterminable-destination');
  denied('GIT_EDITOR=vim git commit', 'undeterminable-destination');
  denied('GIT_DIR=/Volumes/E/.git git log --output=/Volumes/D/scratch/l', 'undeterminable-destination');
  denied('GIT_DIR=/Volumes/E/.git git init /Volumes/D/scratch/r', 'undeterminable-destination');
  for (const command of ['GIT_TERMINAL_PROMPT=0 git push origin main', 'GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=b GIT_AUTHOR_DATE=2026-10-19 git commit -m x', 'GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=b git commit -m x',
    'GIT_PAGER=cat git add .', 'GIT_EDITOR=true git rebase --continue', 'GIT_EDITOR=: git commit']) allowed(command);
  allowed('git commit -m x', undefined, { env: { PATH: '/usr/bin', HOME: home, GIT_PAGER: 'cat', GIT_AUTHOR_NAME: 'a', GIT_TERMINAL_PROMPT: '0', GIT_EDITOR: 'true' } });
  // read-only forms without an output file do not write, so an override alone does not refuse them
  denied('GIT_DIR=/Volumes/E/.git git status', 'undeterminable-destination'); // round 4 amendment: every invocation, read-only forms included
});

test('round 4 amendment: GIT_* and -c/--config-env refusals apply to EVERY git invocation, read-only forms included', () => {
  for (const command of ['GIT_DIR=/Volumes/E/.git git log', 'GIT_WORK_TREE=/Volumes/E git show HEAD', 'GIT_CONFIG_COUNT=1 git rev-parse HEAD', 'GIT_CONFIG_GLOBAL=/Volumes/E/c git cat-file -p HEAD', 'GIT_INDEX_FILE=/Volumes/E/i git ls-files',
    'GIT_ALTERNATE_OBJECT_DIRECTORIES=/x git ls-tree HEAD', 'export GIT_DIR=/Volumes/E/.git; git log', 'env GIT_DIR=/Volumes/E/.git git log', 'git --version; GIT_DIR=/x git log', 'GIT_OPTIONAL_LOCKS=1 git status', 'GIT_OPTIONAL_LOCKS=$X git status',
    'GIT_OPTIONAL_LOCKS=false git log', 'git -c core.pager=x log', 'git -c user.name=x show HEAD', 'git --config-env=core.pager=V log', 'git --git-dir=/Volumes/E/.git log', 'git --work-tree=/Volumes/E status', 'git -c x.y=z remote -v', 'git -c x.y=z --version']) {
    denied(command, 'undeterminable-destination');
  }
  denied('git log', 'undeterminable-destination', undefined, { env: { GIT_DIR: '/Volumes/E/.git' } });
  denied('git status', 'undeterminable-destination', undefined, { env: { GIT_OPTIONAL_LOCKS: '1' } });
  allowed('git log', undefined, { env: { GIT_OPTIONAL_LOCKS: '0', GIT_PAGER: 'cat' } });
});

test('round 4 amendment: status, diff, describe and other index-refreshing forms are repo-local writers unless optional locks are off', () => {
  const here = '/Volumes/D/claude/app';
  const touching = ['git status', 'git status -s', 'git diff', 'git diff HEAD~1', 'git diff --stat', 'git diff-files', 'git diff-index HEAD', 'git describe --dirty', 'git describe --tags', 'git ls-files -m', 'git ls-files --modified', 'git ls-files -d', 'git ls-files --deleted',
    'git ls-files --mod', 'git ls-files -mo', 'git ls-files $X', 'git reflog expire --all', 'git reflog delete HEAD@{1}', 'git reflog $X'];
  for (const command of touching) {
    // physical closure required: git is spawned in the repository, and a repository off the agent volume is denied
    const spy = fakeGit();
    assert.equal(check(command, here, { gitRunner: spy }), null, command);
    assert.equal(spy.calls.length, 1, `${command}: the closure check runs`);
    denied(command, 'non-agent-volume', '/Volumes/E');
    denied(command, 'non-agent-volume', home);
    denied(command, 'undeterminable-destination', null);
    denied(command, 'non-agent-volume', here, { gitRunner: fakeGit({ repos: { [here]: { gitDir: '/Volumes/E/.git' } } }) });
    denied(command, 'non-agent-volume', here, { gitRunner: fakeGit({ repos: { [here]: { top: '/Volumes/E/wt' } } }) });
    denied(command, 'non-agent-volume', here, { gitRunner: fakeGit({ repos: { [here]: { common: '/Volumes/E/c' } } }) });
    denied(command, 'undeterminable-destination', here, { gitRunner: fakeGit({ status: 128 }) });
  }
  // optional locks off: --no-optional-locks, or GIT_OPTIONAL_LOCKS=0 (prefix, export, env, hook process env)
  for (const cwd of [here, '/Volumes/E', home, null]) {
    const spy = fakeGit();
    for (const command of ['git --no-optional-locks status', 'git --no-optional-locks diff', 'git --no-optional-locks describe --dirty', 'git --no-optional-locks ls-files -m', 'GIT_OPTIONAL_LOCKS=0 git status', 'GIT_OPTIONAL_LOCKS=0 git diff',
      'export GIT_OPTIONAL_LOCKS=0; git status', 'env GIT_OPTIONAL_LOCKS=0 git describe --dirty', 'git -C /Volumes/E --no-optional-locks status']) {
      assert.equal(check(command, cwd, { gitRunner: spy }), null, `${command} from ${cwd}`);
    }
    assert.equal(check('git status', cwd, { gitRunner: spy, env: { GIT_OPTIONAL_LOCKS: '0' } }), null);
    assert.equal(spy.calls.length, 0, 'no spawn when optional locks are off');
  }
  denied('GIT_OPTIONAL_LOCKS=0 GIT_DIR=/x git status', 'undeterminable-destination');
  denied('GIT_OPTIONAL_LOCKS=00 git status', 'undeterminable-destination');
  denied('git --no-optional-locks -c core.pager=x status', 'undeterminable-destination');
  denied('git --no-optional-locks diff --output=/Volumes/E/d', 'non-agent-volume'); // output destinations still apply
  // forms that never touch the index stay free (no spawn): see the pure-reader test above; diff --no-index never reads a repository
  const free = fakeGit();
  for (const command of ['git ls-files', 'git ls-files -o', 'git ls-files -s', 'git diff --no-index a b', 'git log', 'git show HEAD', 'git cat-file -p HEAD', 'git ls-tree HEAD', 'git rev-parse HEAD', 'git reflog']) assert.equal(check(command, '/Volumes/E', { gitRunner: free }), null, command);
  assert.equal(free.calls.length, 0);
});

test('round 4 git options: -c, --config-env, --git-dir, --work-tree, --namespace, --exec-path= are refused', () => {
  for (const command of ['git -c user.name=x commit -m x', 'git -c core.hooksPath=/x commit', 'git -ccore.worktree=/Volumes/E add .', 'git -c include.path=/Volumes/E/c add .', 'git -c alias.x=!sh status',
    'git --config-env=core.worktree=V add .', 'git --config-env core.worktree=V add .', 'git --git-dir=/Volumes/E/.git add .', 'git --git-dir /Volumes/E/.git add .', 'git --work-tree=/Volumes/E add .',
    'git --work-tree /Volumes/E add .', 'git --namespace=x push origin main', 'git --exec-path=/x status', 'git -c core.pager=x log', 'git -c x.y=z status', 'git --git-dir=/Volumes/D/scratch/.git status']) {
    denied(command, 'undeterminable-destination');
  }
  allowed('git --no-pager log');
  allowed('git -C /Volumes/D/scratch status');
});

test('round 4 git (c): every output option is a destination on every subcommand', () => {
  for (const command of ['git log --output=/Volumes/E/l', 'git diff --output /Volumes/E/d', 'git show --output=/Volumes/E/s HEAD', 'git log --out=/Volumes/E/l', 'git diff --outp=/Volumes/E/l', 'git diff-tree --output=/Volumes/E/t HEAD',
    'git archive -o /Volumes/E/a.tar HEAD', 'git archive --output=/Volumes/E/a.tar HEAD', 'git archive -o/Volumes/E/a.tar HEAD', 'git bundle create /Volumes/E/b.bundle --all',
    'git format-patch -o /Volumes/E/p HEAD~1', 'git format-patch -o/Volumes/E/p HEAD~1', 'git format-patch --output-directory=/Volumes/E/p HEAD~1', 'git format-patch --output-directory /Volumes/E/p HEAD~1',
    'git init /Volumes/E/r', 'git init --separate-git-dir=/Volumes/E/g /Volumes/D/scratch/r', 'git clone https://example.com/r.git /Volumes/E/r', 'git worktree add /Volumes/E/w', 'git config --file /Volumes/E/c k v']) {
    denied(command, 'non-agent-volume');
  }
  denied('git format-patch HEAD~1', 'non-agent-volume', '/Volumes/E'); // writes patch files into the cwd
  denied('git init', 'non-agent-volume', '/Volumes/E');
  denied('git log --output=$X', 'undeterminable-destination');
  denied('git diff $X --output=/Volumes/D/scratch/d', 'undeterminable-destination');
  denied('git archive -o $X HEAD', 'undeterminable-destination');
  denied('git bundle create $X --all', 'undeterminable-destination');
  denied('git bundle frobnicate x', 'undeterminable-destination');
  for (const command of ['git log --output=/Volumes/D/scratch/l', 'git diff --output /Volumes/D/scratch/d', 'git archive -o /Volumes/D/scratch/a.tar HEAD', 'git bundle create /Volumes/D/scratch/b.bundle --all',
    'git format-patch -o /Volumes/D/scratch/p HEAD~1', 'git format-patch --stdout HEAD~1', 'git init /Volumes/D/scratch/r', 'git archive HEAD', 'git config --file /Volumes/D/scratch/c k v']) allowed(command);
  allowed('git format-patch --stdout HEAD~1', '/Volumes/E');
});

test('round 4 git (d): unmodelled subcommands and code-running options are refused', () => {
  for (const command of ['git frobnicate', 'git submodule update --init', 'git filter-branch x', 'git am x.patch', 'git apply x.patch', 'git notes add -m x', 'git update-ref refs/heads/x HEAD', 'git hash-object -w x',
    'git replace a b', 'git difftool', 'git mergetool', 'git svn fetch', 'git daemon', 'git send-email x', 'git credential fill', 'git $SUB status', 'git rebase -x "make" main', 'git rebase --exec make main',
    'git fetch --upload-pack=x origin', 'git pull --upload-pack x origin', 'git push --receive-pack=x origin', 'git clone -u x https://example.com/r.git /Volumes/D/scratch/r', 'git grep -O x', 'git ls-remote --upload-pack=x origin',
    'git config --global user.name x', 'git config --system x y', 'git config --edit']) {
    denied(command, 'undeterminable-destination');
  }
});

test('round 4 git (4): push/fetch/pull remotes that are local paths must resolve onto the agent volume; network URLs are fine', () => {
  const here = '/Volumes/D/claude/app';
  const remote = (fetchUrl, pushUrl = fetchUrl) => ({ gitRunner: fakeGit({ repos: { [here]: { remotes: [['origin', fetchUrl, pushUrl]] } } }) });
  for (const command of ['git push /Volumes/E/r.git main', 'git push file:///Volumes/E/r.git main', 'git fetch file://localhost/Volumes/E/r', 'git pull /Volumes/E/r main', 'git push ../../../E/r', 'git push --force /Volumes/E/r HEAD',
    'git push -o ci.skip /Volumes/E/r main', 'git push --repo=/Volumes/E/r', 'git push file:///Volumes/D/scratch/../../E/r', 'git push /Volumes/D/scratch/to-internal/r', 'git push file:///Volumes/E/r%20x']) {
    denied(command, undefined);
  }
  denied('git push ext::sh -c x', 'undeterminable-destination');
  denied('git push fd::3', 'undeterminable-destination');
  denied('git push $R main', 'undeterminable-destination');
  for (const command of ['git push /Volumes/D/scratch/r.git main', 'git push file:///Volumes/D/scratch/r.git main', 'git push https://github.com/o/r main', 'git push git@github.com:o/r.git', 'git push ssh://git@host/o/r', 'git push file://remote-host/x',
    'git fetch --all', 'git pull', 'git push', 'git push -u origin main', 'git push -o ci.skip origin main', 'git pull --rebase origin main', 'git pull -s ours origin main', 'git fetch -j4 origin', 'git push nothere main', 'git push ./sub/r.git']) allowed(command);
  // by remote name, resolved through `git remote -v` in the same bounded seam
  denied('git push origin', 'non-agent-volume', undefined, remote('/Volumes/E/r.git'));
  denied('git fetch origin', 'non-agent-volume', undefined, remote('file:///Volumes/E/r.git'));
  denied('git pull', 'non-agent-volume', undefined, remote('/Volumes/E/r.git'));
  denied('git push', 'non-agent-volume', undefined, remote('/Volumes/E/r.git'));
  denied('git push origin main', 'non-agent-volume', undefined, remote('https://github.com/o/r.git', '/Volumes/E/r.git')); // pushurl differs
  denied('git push origin', 'non-agent-volume', undefined, remote('/Volumes/D/scratch/r', '/Volumes/D/scratch/to-internal/r'));
  denied('git fetch --all', 'non-agent-volume', undefined, remote('../../../E/r'));
  denied('git push origin', 'undeterminable-destination', undefined, remote('ext::sh -c x'));
  denied('git push origin', 'undeterminable-destination', undefined, { gitRunner: fakeGit({ remoteStatus: 1 }) }); // remote -v failed
  allowed('git fetch origin', undefined, remote('https://github.com/o/r.git', '/Volumes/E/r.git')); // fetch ignores the push URL
  allowed('git push origin', undefined, remote('/Volumes/D/scratch/r.git'));
  allowed('git push origin main', undefined, remote('git@github.com:o/r.git'));
  allowed('git pull origin main', undefined, remote('./sub/r'));
  // an unknown name is a relative path to git: it is checked as one
  allowed('git push elsewhere main', here, { gitRunner: fakeGit({ repos: { [here]: { remotes: [] } } }) }); // relative to the repository on D
  denied('git push elsewhere main', 'non-agent-volume', `${here}/..`); // a cwd that still contains ".." is not accepted as the repository directory
});

test('round 4 find: -L/-H/-follow with -delete is refused; the start-path check stays without them', () => {
  for (const command of ['find -L /Volumes/D/scratch -name x -delete', 'find -H /Volumes/D/scratch -delete', 'find /Volumes/D/scratch -follow -delete', 'find /Volumes/D/scratch -L -delete', 'find -L . -delete', 'find -P -L /Volumes/D/scratch -delete',
    'find -L /Volumes/D/scratch -type f -name x -delete']) {
    denied(command, 'undeterminable-destination');
  }
  allowed('find -P /Volumes/D/scratch -delete'); // the last of -H/-L/-P wins
  allowed('find -L /Volumes/D/scratch -name x'); // no -delete: nothing is written
  allowed('find /Volumes/D/scratch -name x -delete');
  allowed('find -L -P /Volumes/D/scratch -delete');
  denied('find /Volumes/E/cache -delete', 'non-agent-volume');
  denied('find -P /Volumes/E/cache -delete', 'non-agent-volume');
  denied(`find ${cache} -delete`, 'internal-heavy-path');
});

test('round 4 find: nested -exec/-ok is analysed in a forked scope; a sibling or following command does not inherit it', () => {
  denied('find /Volumes/E -exec env -C /Volumes/D true \\; ; touch rel', 'non-agent-volume', '/Volumes/E');
  denied('find /Volumes/D/scratch -exec env -C /Volumes/D true \\; ; touch rel', 'non-agent-volume', '/Volumes/E');
  denied('find /Volumes/D/scratch -exec env -C /Volumes/D true \\; -exec touch rel \\;', 'non-agent-volume', '/Volumes/E');
  allowed('find /Volumes/D/scratch -exec env -C /Volumes/E true \\; -exec touch rel \\;'); // env -C inside the first -exec does not move the second
  allowed('find /Volumes/D/scratch -exec env -C /Volumes/E true \\; ; touch rel');
  allowed('find /Volumes/D/scratch -exec env X=/Volumes/E true \\; -exec touch rel \\;');
  denied('find /Volumes/D/scratch -exec touch /Volumes/E/x \\;', 'non-agent-volume');
  denied('find /Volumes/D/scratch -ok touch /Volumes/E/x \\;', 'non-agent-volume');
  denied('find /Volumes/D/scratch -exec env -C /Volumes/E touch rel \\;', 'non-agent-volume');
});

// ---------------------------------------------------------------------------------------------------------------
// Round 5 (Astra BLOCK on 196c514f): worktree closure, config -f forms, effective probe env, init/reinit. DATA ONLY.
test('round 5 worktree: the source repository and every affected worktree root need physical closure', () => {
  const here = '/Volumes/D/claude/app';
  const listed = (...paths) => ({ gitRunner: fakeGit({ repos: { [here]: { worktrees: paths } } }) });
  // 1. the source repository (-C) is closed first: .git/worktrees/<name> is written there
  denied('git -C /Volumes/E/repo worktree add --detach /Volumes/D/w HEAD', 'non-agent-volume');
  denied('git -C /Volumes/E/repo worktree add -b br /Volumes/D/w', 'non-agent-volume');
  denied('git worktree add /Volumes/D/scratch/w HEAD', 'non-agent-volume', '/Volumes/E');
  denied('git worktree add /Volumes/D/scratch/w HEAD', 'non-agent-volume', home);
  denied('git worktree add /Volumes/D/scratch/w', 'undeterminable-destination', null);
  for (const patch of [{ gitDir: '/Volumes/E/g' }, { common: '/Volumes/E/c' }, { top: '/Volumes/E/t' }]) {
    for (const command of ['git worktree add /Volumes/D/scratch/w HEAD', 'git worktree prune', 'git worktree lock /Volumes/D/scratch/wt', 'git worktree repair', 'git worktree move /Volumes/D/scratch/wt /Volumes/D/scratch/wt2']) {
      denied(command, 'non-agent-volume', here, { gitRunner: fakeGit({ repos: { [here]: { ...patch, worktrees: [here, '/Volumes/D/scratch/wt'] } } }) });
    }
  }
  denied('git worktree add /Volumes/D/scratch/w', 'undeterminable-destination', here, { gitRunner: fakeGit({ status: 128 }) });
  allowed(`git worktree add ${ext}/w HEAD`);
  allowed('git worktree add --detach -b b wt main', ext);
  denied('git worktree add --detach -b b ../wt main', 'repo-create-outside-run-root', ext); // ".." is never guessed
  denied('git worktree add /Volumes/E/w HEAD', 'non-agent-volume');
  // 2. add: the new PATH; move: both paths; remove: the target worktree's resolved root
  allowed('git worktree move /Volumes/D/scratch/wt /Volumes/D/scratch/wt2', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree move /Volumes/D/scratch/wt /Volumes/E/wt2', 'non-agent-volume', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree move /Volumes/E/wt /Volumes/D/scratch/wt2', 'non-agent-volume', here, listed(here, '/Volumes/E/wt')); // the OLD root is on E
  denied('git worktree move wt /Volumes/D/scratch/wt2', 'non-agent-volume', here, listed(here, '/Volumes/E/wt')); // git's unique-suffix match
  denied('git worktree move /Volumes/D/scratch/to-internal/wt /Volumes/D/scratch/wt2', 'non-agent-volume', here, listed(here, '/Volumes/D/scratch/to-internal/wt')); // resolves to the internal drive
  denied('git worktree move /Volumes/D/scratch/nope /Volumes/D/scratch/wt2', 'undeterminable-destination', here, listed(here, '/Volumes/D/scratch/wt'));
  allowed('git worktree remove /Volumes/D/scratch/wt', here, listed(here, '/Volumes/D/scratch/wt'));
  allowed('git worktree remove --force /Volumes/D/scratch/wt', here, listed(here, '/Volumes/D/scratch/wt'));
  allowed('git worktree remove wt', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree remove /Volumes/E/wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt'));
  denied('git worktree remove wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt'));
  denied('git worktree remove -f -f wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt'));
  denied('git worktree remove /Volumes/D/scratch/to-internal/wt', 'non-agent-volume', here, listed(here, '/Volumes/D/scratch/to-internal/wt'));
  denied('git worktree remove /Volumes/D/scratch/../../E/wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt')); // the root that resolves onto E
  denied('git worktree remove ../x', 'undeterminable-destination', here, listed(here, '/Volumes/D/scratch/wt')); // not a listed worktree
  denied('git worktree remove $W', 'undeterminable-destination', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree remove /Volumes/D/scratch/wt', 'undeterminable-destination', here, { gitRunner: fakeGit({ repos: { [here]: { worktrees: [] } } }) });
  denied('git worktree remove /Volumes/D/scratch/wt', 'undeterminable-destination', here, { gitRunner: (args, options) => (args[0] === 'worktree' ? { status: 128, stdout: '' } : fakeGit()(args, options)) }); // listing failed
  denied('git worktree remove a b', 'undeterminable-destination');
  denied('git worktree remove', 'undeterminable-destination');
  allowed('git worktree lock --reason x /Volumes/D/scratch/wt', here, listed(here, '/Volumes/D/scratch/wt'));
  allowed('git worktree unlock /Volumes/D/scratch/wt', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree lock /Volumes/E/wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt'));
  denied('git worktree unlock /Volumes/E/wt', 'non-agent-volume', here, listed(here, '/Volumes/E/wt'));
  allowed('git worktree prune');
  allowed('git worktree prune --dry-run -v --expire now');
  denied('git worktree prune extra', 'undeterminable-destination');
  allowed('git worktree repair', here, listed(here, '/Volumes/D/scratch/wt'));
  allowed('git worktree repair /Volumes/D/scratch/wt', here, listed(here, '/Volumes/D/scratch/wt'));
  denied('git worktree repair', 'non-agent-volume', here, listed(here, '/Volumes/E/wt')); // repairs every worktree: all roots checked
  denied('git worktree repair /Volumes/E/wt', 'non-agent-volume', here, listed(here, '/Volumes/D/scratch/wt'));
  // any unmodelled worktree action is refused
  for (const command of ['git worktree frobnicate', 'git worktree $A', 'git worktree move-all x']) denied(command, 'undeterminable-destination');
});

test('round 5 config: -f/--file in every form is a destination, --global/--system writes are refused, --blob is read-only', () => {
  for (const command of ['git config -f /Volumes/E/c k v', 'git config -f/Volumes/E/c k v', 'git config --file /Volumes/E/c k v', 'git config --file=/Volumes/E/c k v', 'git config --fil=/Volumes/E/c k v',
    'git config --fil /Volumes/E/c k v', 'git config -lf /Volumes/E/c', 'git config -lf/Volumes/E/c', 'git config --get -f/Volumes/E/c k', 'git config k v -f /Volumes/E/c', 'git config k v --file=/Volumes/E/c', 'git config -f ../../../E/c k v',
    'git config --get-all --file /Volumes/E/c k', 'git config -f ' + cache + '/c k v', 'git config -f /Volumes/D/scratch/../../E/c k v']) {
    const found = denied(command, undefined);
    assert.ok(['non-agent-volume', 'internal-heavy-path'].includes(found.rule), `${command}: ${found.rule}`);
  }
  denied('git config -fE.cfg k v', 'non-agent-volume', '/Volumes/E'); // attached relative value resolved against the cwd
  denied('git config -f $F k v', 'undeterminable-destination');
  denied('git config -f$F k v', 'undeterminable-destination');
  denied('git config --file=$F k v', 'undeterminable-destination');
  denied('git config $K v', 'undeterminable-destination');
  denied('git config k $V', 'undeterminable-destination');
  denied('git config -f', 'undeterminable-destination');
  denied('git config --file', 'undeterminable-destination');
  for (const command of ['git config -f /Volumes/D/scratch/c k v', 'git config -f/Volumes/D/scratch/c k v', 'git config --file=/Volumes/D/scratch/c k v', 'git config --file /Volumes/D/scratch/c --get k', 'git config -lf /Volumes/D/scratch/c',
    'git config -fe.cfg k v', 'git config --fil=/Volumes/D/scratch/c k v']) allowed(command);
  // --global/--system: a write is not a repository destination; reads are fine
  for (const command of ['git config --global user.name x', 'git config --system core.x y', 'git config --global --unset user.name', 'git config --global --add a.b c', 'git config --system --replace-all a.b c', 'git config --global --unset-all x',
    'git config --global --rename-section a b', 'git config --global --remove-section a', 'git config --global user.name x --local']) {
    denied(command, 'undeterminable-destination');
  }
  for (const command of ['git config --global --get user.name', 'git config --global --list', 'git config --global user.name', 'git config --system --get-all x', 'git config --global -l', 'git config --global --get-regexp a']) allowed(command);
  // --blob is read-only
  allowed('git config --blob HEAD:cfg --get k');
  allowed('git config --blob=HEAD:cfg --list');
  allowed('git config --blob HEAD:cfg k');
  denied('git config --blob HEAD:cfg k v', 'undeterminable-destination');
  denied('git config --blob HEAD:cfg --unset k', 'undeterminable-destination');
  // an editor is never run
  for (const command of ['git config --edit', 'git config -e', 'git config --global -e', 'git config -le', 'git config --ed']) denied(command, 'undeterminable-destination');
  // repository writes still need closure
  denied('git config user.name x', 'non-agent-volume', '/Volumes/E');
  allowed('git config user.name x');
  allowed('git config --local --unset user.name');
});

test('round 5 probe env: the rev-parse probe sees the EFFECTIVE HOME and XDG_CONFIG_HOME for this command', () => {
  const callOf = (command, extra = {}) => { const spy = fakeGit(); const found = check(command, undefined, { gitRunner: spy, ...extra }); return { found, call: spy.calls[0], spy }; };
  // inherited values are kept (not silently replaced)
  let r = callOf('git commit -m x', { env: { HOME: '/Users/other', XDG_CONFIG_HOME: '/Users/other/.xdg', PATH: '/usr/bin' } });
  assert.equal(r.found, null);
  assert.equal(r.call.home, '/Users/other');
  assert.equal(r.call.xdgConfigHome, '/Users/other/.xdg');
  // nothing inherited: the seam home, and no XDG_CONFIG_HOME invented
  r = callOf('git commit -m x');
  assert.equal(r.call.home, home);
  assert.equal(r.call.xdgConfigHome, undefined);
  // the command's own values win: prefix assignment, export, env, tracked earlier assignment
  for (const command of ['HOME=/Volumes/D/scratch/h git commit -m x', 'export HOME=/Volumes/D/scratch/h; git commit -m x', 'env HOME=/Volumes/D/scratch/h git commit -m x', 'HOME=/Volumes/D/scratch/h; git commit -m x']) {
    r = callOf(command, { env: { HOME: '/Users/other' } });
    assert.equal(r.found, null, command);
    assert.equal(r.call.home, '/Volumes/D/scratch/h', command);
  }
  for (const command of ['XDG_CONFIG_HOME=/Volumes/D/xdg git commit -m x', 'export XDG_CONFIG_HOME=/Volumes/D/xdg; git commit -m x', 'env XDG_CONFIG_HOME=/Volumes/D/xdg git commit -m x']) {
    r = callOf(command, { env: { XDG_CONFIG_HOME: '/Users/other/.xdg' } });
    assert.equal(r.found, null, command);
    assert.equal(r.call.xdgConfigHome, '/Volumes/D/xdg', command);
  }
  // a value the command sets that cannot be resolved literally is refused, never dropped
  for (const command of ['HOME=$X git commit -m x', 'XDG_CONFIG_HOME=$X git commit -m x', 'HOME=/a$X git add .', 'XDG_CONFIG_HOME="$(pwd)/x" git add .', 'export HOME=$X; git commit -m x', 'export XDG_CONFIG_HOME=$X; git push origin main',
    'HOME=$X git status', 'HOME=$X git worktree remove w', 'HOME=$X git init /Volumes/D/scratch/fresh', 'XDG_CONFIG_HOME=$X git push origin', 'HOME=$X git worktree add /Volumes/D/scratch/w']) {
    const spy = fakeGit();
    denied(command, 'undeterminable-destination', undefined, { gitRunner: spy });
    assert.equal(spy.calls.length, 0, `${command}: git is not spawned with a guessed environment`);
  }
  // the remote and worktree probes get the same effective environment
  const here = '/Volumes/D/claude/app';
  const spy = fakeGit({ repos: { [here]: { remotes: [['origin', 'https://github.com/o/r.git', 'https://github.com/o/r.git']] } } });
  assert.equal(check('HOME=/Volumes/D/h XDG_CONFIG_HOME=/Volumes/D/x git push origin', undefined, { gitRunner: spy }), null);
  assert.ok(spy.calls.length >= 2 && spy.calls.every((call) => call.home === '/Volumes/D/h' && call.xdgConfigHome === '/Volumes/D/x'));
  // read-only pure readers never spawn, so an unresolved HOME does not matter to them
  allowed('HOME=$X git log');
});

test('round 5 init: a target that already holds a repository is reinitialised through its existing gitdir/commondir, which must be on D', () => {
  const target = '/Volumes/D/scratch/hasgit';
  const repo = (patch) => ({ gitRunner: fakeGit({ repos: { [target]: patch } }) });
  // existing .git (dir or gitdir: file): redirected gitdir/commondir/work tree is denied
  denied(`git init ${target}`, 'non-agent-volume', undefined, repo({ gitDir: '/Volumes/E/g' }));
  denied(`git init ${target}`, 'non-agent-volume', undefined, repo({ common: '/Volumes/E/c' }));
  denied(`git init ${target}`, 'non-agent-volume', undefined, repo({ top: '/Volumes/E/t' }));
  denied(`git init ${target}`, 'non-agent-volume', undefined, repo({ gitDir: '/Volumes/D/scratch/to-internal/g' }));
  denied(`git init --bare ${target}`, 'non-agent-volume', undefined, repo({ common: `${home}/work/c` }));
  denied(`git init ${target}`, 'undeterminable-destination', undefined, repo({ notRepo: true })); // .git exists but git cannot read it: a broken or redirected reinit
  denied(`git init ${target}`, 'undeterminable-destination', undefined, { gitRunner: fakeGit({ throws: Object.assign(new Error('x'), { code: 'ETIMEDOUT' }) }) });
  denied(`git init ${target}`, 'undeterminable-destination', undefined, { gitRunner: fakeGit({ out: 'garbage\n' }) });
  allowed(`git init ${target}`);
  allowed(`git init --bare ${target}`);
  // the working directory as the target, and a target inside an existing repository
  denied('git init', 'non-agent-volume', '/Volumes/D/claude/app', { gitRunner: fakeGit({ repos: { '/Volumes/D/claude/app': { gitDir: '/Volumes/E/g' } } }) });
  denied('git init /Volumes/D/scratch/fresh', 'non-agent-volume', undefined, { gitRunner: fakeGit({ repos: { '/Volumes/D/scratch/fresh': { top: '/Volumes/E/outer' } } }) }); // inside a repo whose root is on E
  allowed('git init', '/Volumes/D/claude/app');
  // a fresh init (no .git entry, git says "not a repository") checks only the target
  const fresh = '/Volumes/D/scratch/fresh';
  allowed(`git init ${fresh}`, undefined, { gitRunner: fakeGit({ repos: { [fresh]: { notRepo: true } } }) });
  allowed(`git init --bare ${fresh}`, undefined, { gitRunner: fakeGit({ repos: { [fresh]: { notRepo: true } } }) });
  denied(`git init ${fresh}`, 'undeterminable-destination', undefined, { gitRunner: fakeGit({ status: 1 }) }); // a failure other than "not a repository"
  denied(`git init ${fresh}`, 'undeterminable-destination', undefined, { gitRunner: fakeGit({ throws: Object.assign(new Error('x'), { code: 'ENOENT' }) }) });
  const spy = fakeGit();
  allowed('git init /Volumes/D/scratch/brand-new-dir', undefined, { gitRunner: spy }); // does not exist yet: nothing to reinitialise
  assert.equal(spy.calls.length, 0);
  // --separate-git-dir is a destination, in both spellings
  denied(`git init --separate-git-dir=/Volumes/E/g ${fresh}`, 'non-agent-volume');
  denied(`git init --separate-git-dir /Volumes/E/g ${fresh}`, 'non-agent-volume');
  denied(`git init --separate-git-dir=$G ${fresh}`, 'undeterminable-destination');
  denied(`git init --separate-git-dir=${cache}/g ${fresh}`, 'internal-heavy-path');
  allowed(`git init --separate-git-dir=/Volumes/D/scratch/g ${fresh}`, undefined, { gitRunner: fakeGit({ repos: { [fresh]: { notRepo: true } } }) });
  denied('git init /Volumes/E/r', 'non-agent-volume');
  denied('git init', 'non-agent-volume', '/Volumes/E');
});

// Round 6 (Astra BLOCK on c7f760df097b82af): git config option abbreviation, env -i / unexported HOME and XDG_CONFIG_HOME. DATA ONLY.
test('round 6 config: long options are canonicalised (git accepts unique prefixes) before any read/write/scope classification', () => {
  for (const command of ['git config --glo user.name x', 'git config --globa=1 user.name x', 'git config --sys core.x y', 'git config --g user.name x', 'git config --gl --unset user.name', 'git config user.name x --glo',
    'git config --glo --add a.b c', 'git config --syst --replace-all a.b c', 'git config --global=1 user.name x', 'git config --bogus k v', 'git config --no k v', 'git config --no-glo k v', 'git config --= k v']) {
    denied(command, 'undeterminable-destination');
  }
  // an ambiguous or unknown option refuses even beside a read, because the guard cannot tell which option git would pick
  denied('git config --g --get user.name', 'undeterminable-destination');
  denied('git config --get --nope user.name', 'undeterminable-destination');
  // canonicalised reads stay reads
  for (const command of ['git config --glo --get user.name', 'git config --gl --list', 'git config --sys --get-all x', 'git config --glo --get-r a', 'git config --globa --list', 'git config --glo user.name', 'git config --local --unset user.name',
    'git config --get --default --g user.name', 'git config --blo HEAD:cfg --get k', 'git config --fil /Volumes/D/scratch/c --glo k v']) allowed(command);
  // the value word after a value-taking option is data, never canonicalised
  allowed('git config --get --comment --glo user.name');
  // the editor, in subcommand form and abbreviated
  for (const command of ['git config edit', 'git config --global edit', 'git config --ed', 'git config --edi', 'git config --global --ed']) denied(command, 'undeterminable-destination');
});

test('round 6 env -i: the probe has no HOME or XDG_CONFIG_HOME (not the hook\'s), command-set values reach it', () => {
  const env = { HOME: '/Users/other', XDG_CONFIG_HOME: '/Users/other/.xdg', PATH: '/usr/bin' };
  const callOf = (command, extra = {}) => { const spy = fakeGit(); const found = check(command, undefined, { gitRunner: spy, env, ...extra }); return { found, call: spy.calls[0], spy }; };
  for (const command of ['env -i git -C /Volumes/D/scratch commit -m x', 'env --ignore-environment git -C /Volumes/D/scratch commit -m x', "env -i sh -c 'git -C /Volumes/D/scratch commit -m x'", "env -i bash -c 'git -C /Volumes/D/scratch commit -m x'"]) {
    const r = callOf(command);
    assert.equal(r.found, null, command);
    assert.equal(r.call.home, undefined, `${command}: the child has no HOME, so the probe gets none`);
    assert.equal(r.call.xdgConfigHome, undefined, command);
    assert.ok(!('HOME' in gitChildEnv(r.call)) && !('XDG_CONFIG_HOME' in gitChildEnv(r.call)), `${command}: the spawned environment omits HOME instead of setting ""`);
  }
  assert.equal(gitChildEnv({ home: '/Volumes/D/h', xdgConfigHome: '/Volumes/D/x', path: '/usr/bin' }).HOME, '/Volumes/D/h');
  // values the command gives env -i reach the probe; the other one stays absent
  let r = callOf('env -i HOME=/Volumes/D/h git -C /Volumes/D/scratch commit -m x');
  assert.equal(r.found, null);
  assert.equal(r.call.home, '/Volumes/D/h');
  assert.equal(r.call.xdgConfigHome, undefined);
  r = callOf('env -i XDG_CONFIG_HOME=/Volumes/D/x git -C /Volumes/D/scratch commit -m x');
  assert.equal(r.call.home, undefined);
  assert.equal(r.call.xdgConfigHome, '/Volumes/D/x');
  // a plain assignment inside a child that has no HOME is not exported either
  denied("env -i sh -c 'HOME=/Volumes/D/h; git -C /Volumes/D/scratch commit -m x'", 'undeterminable-destination', undefined, { env });
});

test('round 6 probe env: unknown and unexported HOME/XDG_CONFIG_HOME refuse; exported ones are used', () => {
  const env = { PATH: '/usr/bin' };
  const callOf = (command, extra = {}) => { const spy = fakeGit(); const found = check(command, undefined, { gitRunner: spy, env, ...extra }); return { found, call: spy.calls[0], spy }; };
  // read may have set HOME to anything
  for (const command of ['read HOME; git -C /Volumes/D/scratch commit -m x', 'read XDG_CONFIG_HOME; git -C /Volumes/D/scratch commit -m x']) {
    const spy = fakeGit();
    denied(command, 'undeterminable-destination', undefined, { gitRunner: spy, env });
    assert.equal(spy.calls.length, 0, command);
  }
  // an earlier eval/source may have set HOME even though the name is not tracked
  const evalSpy = fakeGit();
  denied("eval 'true'; git init /Volumes/D/scratch/hasgit", 'undeterminable-destination', undefined, { gitRunner: evalSpy, env });
  assert.equal(evalSpy.calls.length, 0);
  // a plain assignment that is not exported (and not already in the environment) does not reach git
  for (const command of ['XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x', 'HOME=/Volumes/D/h; git -C /Volumes/D/scratch commit -m x', 'declare XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x',
    'readonly XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x']) {
    const spy = fakeGit();
    denied(command, 'undeterminable-destination', undefined, { gitRunner: spy, env });
    assert.equal(spy.calls.length, 0, `${command}: git is not spawned with a guessed environment`);
  }
  // exported in any way, or a prefix assignment: the value is what git sees
  for (const command of ['XDG_CONFIG_HOME=/Volumes/D/x; export XDG_CONFIG_HOME; git -C /Volumes/D/scratch commit -m x', 'export XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x',
    'declare -x XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x', 'XDG_CONFIG_HOME=/Volumes/D/x; XDG_CONFIG_HOME=/Volumes/D/y git -C /Volumes/D/scratch commit -m x',
    'env XDG_CONFIG_HOME=/Volumes/D/x git -C /Volumes/D/scratch commit -m x']) {
    const r = callOf(command);
    assert.equal(r.found, null, command);
    assert.match(r.call.xdgConfigHome, /^\/Volumes\/D\/[xy]$/, command);
  }
  // a name the hook already has exported: a plain assignment modifies the exported variable
  const inherited = callOf('XDG_CONFIG_HOME=/Volumes/D/x; git -C /Volumes/D/scratch commit -m x', { env: { PATH: '/usr/bin', XDG_CONFIG_HOME: '/Users/other/.xdg' } });
  assert.equal(inherited.found, null);
  assert.equal(inherited.call.xdgConfigHome, '/Volumes/D/x');
  // other variables are not affected by the export tracking
  allowed('FOO=1; git -C /Volumes/D/scratch commit -m x', undefined, { env });
});

// Round 6b (review BLOCKs): option values are never classified; export state is only set by certain, modelled export forms. DATA ONLY.
test('round 6b config: classification comes from parsed option nodes, never from a value word', () => {
  // the value of a value-taking option (separate or after "=") is consumed by its option: these are GLOBAL WRITES
  for (const command of ['git config --glo --comment --list user.name x', 'git config --glo --comment=--get user.name x', 'git config --glo --default --get-all user.name x', 'git config --glo --type --list user.name x',
    'git config --glo -t --list user.name x', 'git config --glo --value --get user.name x', 'git config --glo --url --show-origin k v', 'git config --glo --blob --list k v', 'git config --sys --type=--get k v']) {
    denied(command, 'undeterminable-destination');
  }
  // -t consumes its value: "--list" is a type, not a list, so this is a repository write (denied on E, allowed on D)
  denied('git config -t --list user.name x', 'non-agent-volume', '/Volumes/E');
  denied('git config --comment --get user.name x', 'non-agent-volume', '/Volumes/E');
  allowed('git config -t --list user.name x');
  // a write flag that is only a value does not turn a read into a write
  allowed('git config --glo --comment --unset user.name');
  allowed('git config --glo --default --add user.name');
  // real flags still classify
  denied('git config --glo --comment x --unset user.name', 'undeterminable-destination');
  allowed('git config --glo --comment x --get user.name');
  // a value-taking option with no value refuses
  denied('git config --glo --comment', 'undeterminable-destination');
  denied('git config -t', 'undeterminable-destination');
  // after "--" every word is an operand, not an option
  denied('git config -- --global x', 'non-agent-volume', '/Volumes/E');
});

test('round 6b export: only a certain, modelled export form sets the exported state; anything else is UNKNOWN and refuses', () => {
  const env = { HOME: '/Users/other', PATH: '/usr/bin' };
  const git = 'git -C /Volumes/D/scratch commit -m x';
  for (const command of ['false && export XDG_CONFIG_HOME; ' + git, 'XDG_CONFIG_HOME=/Volumes/D/x; false && export XDG_CONFIG_HOME; ' + git, 'XDG_CONFIG_HOME=/Volumes/D/x; true && export XDG_CONFIG_HOME; ' + git,
    'XDG_CONFIG_HOME=/Volumes/D/x; true || export XDG_CONFIG_HOME; ' + git, 'XDG_CONFIG_HOME=/Volumes/D/x; export XDG_CONFIG_HOME & ' + git,
    // un-export and unmodelled flags
    'export -n HOME; ' + git, 'export -n XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'export -fx XDG_CONFIG_HOME; ' + git, 'export -p XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'XDG_CONFIG_HOME=/Volumes/D/x; export -n XDG_CONFIG_HOME; ' + git,
    'declare -rx XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'declare +x XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'typeset -fx XDG_CONFIG_HOME; ' + git, 'declare -n XDG_CONFIG_HOME=/Volumes/D/x; ' + git,
    // dynamic flag or name
    'export $F XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'export $N=/Volumes/D/x; ' + git, 'declare $F -x XDG_CONFIG_HOME=/Volumes/D/x; ' + git]) {
    const spy = fakeGit();
    denied(command, 'undeterminable-destination', undefined, { gitRunner: spy, env });
    assert.equal(spy.calls.length, 0, `${command}: git is not spawned with a guessed environment`);
  }
  // the modelled literal forms, run for certain, are exported and reach the probe
  const callOf = (command, extra = {}) => { const spy = fakeGit(); const found = check(command, undefined, { gitRunner: spy, env, ...extra }); return { found, call: spy.calls[0] }; };
  for (const command of ['XDG_CONFIG_HOME=/Volumes/D/x; export XDG_CONFIG_HOME; ' + git, 'export XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'declare -x XDG_CONFIG_HOME=/Volumes/D/x; ' + git, 'typeset -x XDG_CONFIG_HOME=/Volumes/D/x; ' + git,
    'XDG_CONFIG_HOME=/Volumes/D/x; export XDG_CONFIG_HOME && ' + git, 'export XDG_CONFIG_HOME=/Volumes/D/x && ' + git]) {
    const r = callOf(command);
    assert.equal(r.found, null, command);
    assert.equal(r.call.xdgConfigHome, '/Volumes/D/x', command);
  }
  // other variables are not affected
  allowed('export -n FOO; ' + git, undefined, { env });
  allowed('false && export FOO; ' + git, undefined, { env });
});
