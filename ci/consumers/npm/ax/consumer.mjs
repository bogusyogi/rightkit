import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'ax-plugin-gates-report-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const SUBPATHS = {
  './manifest': ['loadManifest', 'manifestPathFor', 'manifestSchema'],
  './ax/static': ['fetchLiveTools', 'grepRepoForTokens', 'runStaticChecks'],
  './ax/conformance': ['buildValidArgs', 'evaluateProbe', 'runBlockingCapabilityProbe', 'runConformance', 'tryParseJson'],
  './ax/behavioral': ['evaluateTrial', 'listScenarioFiles', 'loadScenarios', 'runMatrix', 'runScenarios', 'stubTrial'],
  './ax/adversarial': ['openAdversarialSession', 'runAdversarial'],
  './ax/ownership-review': ['OWNERSHIP_REVIEW_LIMITS', 'assertOwnershipReviewPacket', 'buildOwnershipReviewPacket', 'readOwnershipReviewJson', 'validateOwnershipReviewReceipt', 'writeOwnershipReviewPacket'],
  './ax/report': ['aggregateReport', 'gateExitCode', 'renderMarkdown', 'toSarif'],
  './plugin/scaffold': ['buildCliSkillMarkdown', 'buildMcpManifest', 'buildPluginManifest', 'buildSkillMarkdown', 'scaffoldPlugin'],
  './plugin/validate': ['detectLayout', 'lintPluginMetadata', 'loadPinnedSchemas', 'validateRepoLayout'],
  './plugin/portable-core': ['ANTIGRAVITY_PLUGIN_SCHEMA', 'CLIENT_PROJECTION_KINDS', 'PORTABLE_CORE_KIND', 'PORTABLE_CORE_SCHEMA', 'PORTABLE_CORE_SCHEMA_VERSION', 'assemble', 'assemblePortableCore', 'normalizeClientProjections', 'validatePortableCore', 'validatePortableCoreContract', 'validatePortableCoreDirectory', 'validateResourceClosure'],
  './plugin/hooks': ['DEFAULT_PUSH_EVENTS', 'HOOKS_FILE', 'KNOWN_HOOK_EVENTS', 'buildHooksFiles', 'validateHooks'],
  './plugin/skill-gate': ['DESCRIPTION_MAX', 'DESCRIPTION_MIN', 'DESCRIPTION_WARN', 'checkSkillDescription', 'parseSkillFrontmatter'],
  './plugin/marketplace': ['CLAUDE_MARKETPLACE_PATH', 'CLAUDE_MARKETPLACE_SCHEMA', 'CODEX_MARKETPLACE_PATH', 'HOSTS', 'SOURCE_LIST_SCHEMA_VERSION', 'buildClaudeMarketplace', 'buildCodexMarketplace', 'buildMarketplace', 'parseSourceList', 'readSourceList', 'renderMarketplaceFiles', 'resolvePluginRoot', 'validateMarketplace', 'workspaceIsMarketplaceRoot'],
};
const PULSE = 'Message AI chats on paired machines through Pulse chat; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.';
const SCRAPE = 'Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content.';
// A minimal hand-rolled MCP server over stdio: one tool that follows the contract and one that does not.
const FAKE_MCP_SERVER = `import { createInterface } from 'node:readline';
const TOOLS = [
  { name: 'good_tool', description: 'Use this when you need a well-formed conformance fixture. Do not use it for anything else.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['input'], properties: { input: { type: 'string' } } },
    outputSchema: { type: 'object', properties: { status: { type: 'string' }, truncated: { type: 'boolean' }, continuationCursor: { type: ['string', 'null'] }, claimBoundary: { type: 'object', properties: { status: { type: 'string' }, cleanClaimAllowed: { type: 'boolean' }, safeClaims: { type: 'array' }, prohibitedClaims: { type: 'array' }, gaps: { type: 'array' } } } } },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true } },
  { name: 'bad_tool', description: 'Does something.', inputSchema: { type: 'object', additionalProperties: true, required: ['value'], properties: { value: { type: 'string' } } } },
];
const typed = (code, message) => ({ content: [{ type: 'text', text: JSON.stringify({ error: { code, message, retryable: false, remediation: 'retry with a valid argument shape' } }) }], isError: true });
const call = (name, args) => {
  if (name === 'good_tool') {
    const unknown = Object.keys(args ?? {}).filter(key => key !== 'input');
    if (unknown.length) return typed('unknown_field', 'unexpected field(s): ' + unknown.join(', '));
    if (args?.input === undefined) return typed('missing_required', 'missing required field: input');
    const structuredContent = { status: 'ok', truncated: false, continuationCursor: null, claimBoundary: { status: 'clear', cleanClaimAllowed: true, safeClaims: ['fixture result is current'], prohibitedClaims: [], gaps: [] } };
    return { content: [{ type: 'text', text: JSON.stringify(structuredContent) }], structuredContent };
  }
  if (name === 'bad_tool') return { content: [{ type: 'text', text: JSON.stringify({ received: args ?? {} }) }] };
  return typed('unknown_tool', 'no such tool: ' + name);
};
createInterface({ input: process.stdin, terminal: false }).on('line', line => {
  let message; try { message = JSON.parse(line.trim()); } catch { return; }
  if (message.method === 'notifications/initialized') return;
  const respond = value => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: value }) + '\\n');
  if (message.method === 'initialize') return respond({ protocolVersion: '2025-03-26', serverInfo: { name: 'fixture', version: '0.1.0' }, capabilities: { tools: {} } });
  if (message.method === 'tools/list') return respond({ tools: TOOLS });
  if (message.method === 'resources/list') return respond({ resources: [] });
  if (message.method === 'prompts/list') return respond({ prompts: [] });
  if (message.method === 'tools/call') return respond(call(message.params?.name, message.params?.arguments));
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found: ' + message.method } }) + '\\n');
});
`;
// Fixture failures report fixed scenario names only: never serialize an untrusted tool or CLI error.
let stage = 'artifact-provenance';
let scratch = null;
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  // The package root export is a file one directory below the package root, which the provenance check needs.
  const entries = Object.fromEntries(Object.keys(SUBPATHS).map(subpath => [`@rightkit/ax/${subpath.slice(2)}`, import.meta.resolve(`@rightkit/ax/${subpath.slice(2)}`)]));
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/ax',
    rootResolvedUrl: entries['@rightkit/ax/ax/static'], entries,
  });
  result.artifact = provenance.artifact;
  const load = async subpath => import(provenance.entries[`@rightkit/ax/${subpath.slice(2)}`]);
  const [manifestApi, staticApi, conformanceApi, behavioralApi, adversarialApi, ownershipApi, reportApi, scaffoldApi, validateApi, portableApi, hooksApi, skillGateApi, marketplaceApi] = await Promise.all(Object.keys(SUBPATHS).map(load));
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(entries['@rightkit/ax/ax/static']))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const bin = path.join(packageRoot, 'cli', 'right-ax.mjs');
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };
  scratch = realpathSync(mkdtempSync(path.join(os.tmpdir(), 'rk-ax-consumer-')));
  let counter = 0;
  const dir = label => { const target = path.join(scratch, `${label}-${++counter}`); mkdirSync(target, { recursive: true }); return target; };
  const put = (root, rel, text) => { const target = path.join(root, ...rel.split('/')); mkdirSync(path.dirname(target), { recursive: true }); writeFileSync(target, text); return target; };
  const cli = (args, options = {}) => spawnSync(process.execPath, [bin, ...args], { encoding: 'utf8', windowsHide: true, timeout: 120_000, ...options });
  const errorRules = findings => findings.filter(item => item.severity === 'error').map(item => item.ruleId);
  // The fixture MCP server lives in an artifact root outside the repository under test, as the gates require.
  const artifactRoot = dir('artifact');
  put(artifactRoot, 'fixture-mcp-server.mjs', FAKE_MCP_SERVER);
  const mcpManifest = (extraAx = {}) => ({ schemaVersion: 2, surfaces: { mcp: { command: 'node', args: ['fixture-mcp-server.mjs'], cwd: '${ARTIFACT_ROOT}' } }, ax: { gates: {}, ...extraAx } });
  const evidence = {};

  await run('installed-manifest-and-exact-export-surface', async () => {
    assert.equal(installed.name, '@rightkit/ax');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.deepEqual(Object.keys(installed.exports).sort(), Object.keys(SUBPATHS).sort());
    assert.deepEqual(Object.fromEntries(Object.entries(installed.bin).map(([key, target]) => [key, target.replace(/^\.\//, '')])), { 'right-ax': 'cli/right-ax.mjs' });
    for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) assert.equal(installed[field], undefined);
    const apis = { './manifest': manifestApi, './ax/static': staticApi, './ax/conformance': conformanceApi, './ax/behavioral': behavioralApi, './ax/adversarial': adversarialApi, './ax/ownership-review': ownershipApi, './ax/report': reportApi, './plugin/scaffold': scaffoldApi, './plugin/validate': validateApi, './plugin/portable-core': portableApi, './plugin/hooks': hooksApi, './plugin/skill-gate': skillGateApi, './plugin/marketplace': marketplaceApi };
    for (const [subpath, names] of Object.entries(SUBPATHS)) assert.deepEqual(Object.keys(apis[subpath]).sort(), names, subpath);
    // Tests and fixtures are not published; the files the bin and the schemas need are.
    const top = readdirSync(packageRoot).sort();
    assert.ok(!top.includes('fixtures') && !top.some(name => name.endsWith('.test.mjs')), 'tests and fixtures are not published');
    for (const file of ['cli/right-ax.mjs', 'adopt.mjs', 'manifest.mjs', 'manifest.schema.json', 'ax/adapter.mjs', 'ax/artifact.mjs', 'lib/version.mjs', 'plugin/schemas/plugin.schema.json', 'plugin/schemas/mcp.schema.json', 'plugin/schemas/pins.json']) assert.ok(existsSync(path.join(packageRoot, ...file.split('/'))), file);
    return { subpaths: Object.keys(SUBPATHS).length, moduleExports: Object.values(SUBPATHS).reduce((sum, names) => sum + names.length, 0), dependencies: 0 };
  });

  await run('installed-cli-version-help-and-argument-rules', async () => {
    const versionRun = cli(['--version']);
    assert.equal(versionRun.status, 0, versionRun.stderr);
    assert.equal(versionRun.stdout.trim(), `right-ax ${version}`);
    const help = cli(['--help']);
    assert.equal(help.status, 0, help.stderr);
    assert.match(help.stdout, /right-ax <command>/);
    assert.ok(!/ERR_MODULE_NOT_FOUND/.test(help.stderr));
    // --help anywhere after a command prints that command's usage and touches nothing.
    const target = path.join(scratch, 'never-created');
    const usage = cli(['plugin', 'init', target, '--help']);
    assert.equal(usage.status, 0, usage.stderr);
    assert.match(usage.stdout, /right-ax plugin init/);
    assert.equal(existsSync(target), false);
    // A missing target, an unknown flag and a stray positional exit 2 before anything is written.
    for (const args of [['plugin', 'init'], ['plugin', 'init', target, '--no-such-flag'], ['plugin', 'init', target, 'extra'], ['plugin', 'init', '-x']]) {
      const refused = cli(args);
      assert.equal(refused.status, 2, args.join(' '));
      assert.equal(existsSync(target), false);
    }
    // The CLI scaffolds and validates a plugin end to end.
    const plugin = dir('cli-plugin');
    const init = cli(['plugin', 'init', plugin, '--cli', 'scraperight', '--skill-description', SCRAPE, '--commands', 'scrape,crawl', '--version', '0.1.0']);
    assert.equal(init.status, 0, init.stderr);
    for (const file of ['plugin.json', '.claude-plugin/plugin.json', '.codex-plugin/plugin.json', 'skills/scraperight/SKILL.md']) assert.ok(existsSync(path.join(plugin, ...file.split('/'))), file);
    assert.equal(existsSync(path.join(plugin, 'mcp.json')), false, 'a CLI skill writes no mcp.json');
    const again = cli(['plugin', 'init', plugin, '--cli', 'scraperight', '--skill-description', SCRAPE]);
    assert.notEqual(again.status, 0, 'an existing plugin.json is refused without --force');
    const validated = cli(['plugin', 'validate', plugin]);
    assert.equal(validated.status, 0, validated.stdout + validated.stderr);
    const vague = cli(['plugin', 'init', dir('vague'), '--cli', 'scraperight', '--skill-description', 'A widget tool.']);
    assert.notEqual(vague.status, 0, 'a description the gate rejects cannot be scaffolded');
    return { version, helpExit: 0, argumentErrorsExit: 2, scaffoldedAndValidated: true };
  });

  await run('manifest-load-and-schema', async () => {
    const repo = dir('manifest');
    assert.equal(manifestApi.manifestPathFor(repo), path.join(repo, '.rightax.json'));
    const schema = manifestApi.manifestSchema();
    assert.equal(typeof schema, 'object');
    assert.ok(schema && Object.keys(schema).length > 0);
    const missing = manifestApi.loadManifest(repo);
    assert.equal(missing.manifest, null);
    assert.equal(missing.path, path.join(repo, '.rightax.json'));
    assert.match(missing.errors[0], /\.rightax\.json not found at /);
    put(repo, '.rightax.json', '{ not json');
    const broken = manifestApi.loadManifest(repo);
    assert.equal(broken.manifest, null);
    assert.match(broken.errors[0], /^\.rightax\.json is not valid JSON: /);
    const valid = { schemaVersion: 2, surfaces: { mcp: { command: 'node', args: ['scripts/server.mjs'], cwd: '${ARTIFACT_ROOT}', tools: 6 } }, plugin: { name: 'widget', skills: ['widget'] }, ax: { scenariosDir: 'evals/ax/scenarios', conformanceExamples: { widget_lookup: { query: 'example' } }, adversarial: { casesDir: 'cases', gate: 'hard' }, gates: { static: 'hard', conformance: 'hard', behavioral: 'report-only' }, claimBoundaryRequired: true } };
    put(repo, '.rightax.json', JSON.stringify(valid));
    const loaded = manifestApi.loadManifest(repo);
    assert.deepEqual(loaded.errors, []);
    assert.deepEqual(loaded.manifest, valid);
    // Schema and semantic errors: a non-object example, and a generated path that is not an exact relative path.
    put(repo, '.rightax.json', JSON.stringify({ schemaVersion: 1, surfaces: {}, ax: { gates: {}, conformanceExamples: { widget_lookup: { query: 'x' }, bad_array: [], bad_string: 'value' } } }));
    const examples = manifestApi.loadManifest(repo);
    assert.equal(examples.manifest, null);
    assert.ok(examples.errors.length >= 2);
    put(repo, '.rightax.json', JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: 's', gates: {}, generatedPaths: ['dist/out.js', '../escape', '/abs', 'a\\b', 'glob/*'] } }));
    const generated = manifestApi.loadManifest(repo);
    assert.equal(generated.manifest, null);
    assert.equal(generated.errors.filter(error => error.includes('generatedPaths')).length, 4);
    const twin = { id: 'Alpha', model: 'same', version: '1', command: 'node', args: [] };
    const matrix = { adapters: [twin, { ...twin, id: 'beta' }], trialsPerAgent: 5, limits: { timeoutMs: 1, maxToolCalls: 0, maxTokens: 0, maxCostUsd: 0 } };
    put(repo, '.rightax.json', JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: 's', gates: {}, matrix } }));
    assert.ok(manifestApi.loadManifest(repo).errors.some(error => error.includes('duplicate execution identity')));
    return { loaded: true, missing: true, invalidJson: true, schemaErrors: true, semanticErrors: true };
  });

  await run('skill-description-gate', async () => {
    assert.deepEqual([skillGateApi.DESCRIPTION_MIN, skillGateApi.DESCRIPTION_WARN, skillGateApi.DESCRIPTION_MAX], [40, 300, 1024]);
    const markdown = description => `---\nname: pulse-bridge\ndescription: ${description}\n---\n\n# Pulse\n`;
    const check = (description, options = { skillDirName: 'pulse-bridge' }) => skillGateApi.checkSkillDescription(markdown(description), options);
    assert.deepEqual(check(PULSE), []);
    assert.deepEqual(check(SCRAPE), []);
    const rules = description => new Set(check(description).map(finding => finding.ruleId));
    assert.ok(rules('A widget tool.').has('skill-description-length'));
    assert.ok(rules('A widget tool.').has('skill-description-opening'));
    assert.ok(rules('Use this when the user asks for things, or other things.').has('skill-description-opening'));
    assert.ok(rules('Tool for scraping pages; use when the user asks to scrape a URL, crawl a site, or extract content.').has('skill-description-opening'));
    assert.ok(rules('Scrape pages into markdown. TODO fix this up and add more detail soon enough.').has('skill-description-vague'));
    assert.ok(rules('Scrape web pages into clean markdown with the scraperight CLI and more; use when the user asks to scrape a URL or crawl a site.').has('skill-description-vague'));
    assert.ok(rules('Scrape web pages into clean markdown with the scraperight CLI.').has('skill-description-triggers'));
    assert.ok(rules('x'.repeat(1100)).has('skill-description-length'));
    for (const finding of check('A widget tool.')) {
      assert.deepEqual(Object.keys(finding).sort(), ['claim', 'evidence', 'remediation', 'ruleId', 'severity']);
      assert.equal(finding.severity, 'error');
    }
    // Frontmatter problems, and a name that differs from the skill directory.
    assert.deepEqual(skillGateApi.checkSkillDescription('# no frontmatter', { skillDirName: 'x' }).map(finding => finding.ruleId), ['skill-frontmatter']);
    assert.deepEqual(check(PULSE, { skillDirName: 'other-dir' }).map(finding => finding.ruleId), ['skill-frontmatter']);
    const parsed = skillGateApi.parseSkillFrontmatter(markdown(PULSE));
    assert.equal(parsed.ok, true);
    assert.deepEqual(parsed.fields, { name: 'pulse-bridge', description: PULSE });
    assert.deepEqual(skillGateApi.parseSkillFrontmatter('no front'), { ok: false, reason: 'no frontmatter block (file must start with ---)' });
    return { passing: 2, rulesChecked: 5, frontmatter: true };
  });

  await run('plugin-scaffold-validate-and-hooks', async () => {
    // A CLI skill: one skill, three manifests, no mcp.json, and it validates with no findings.
    const root = dir('scaffold');
    const scaffold = scaffoldApi.scaffoldPlugin(root, { cli: 'scraperight', skillDescription: SCRAPE, commands: ['scrape', 'crawl'], version: '0.1.0', homepage: 'https://example.invalid/scraperight' });
    assert.equal(scaffold.mcpManifest, null);
    assert.equal(scaffold.hooksManifest, null);
    assert.equal(scaffold.written.length, 4);
    for (const file of scaffold.written) assert.ok(existsSync(file), file);
    const read = (...parts) => JSON.parse(readFileSync(path.join(root, ...parts), 'utf8'));
    const portable = read('plugin.json');
    assert.equal(portable.$schema, 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json');
    assert.equal(portable.name, 'scraperight');
    assert.equal(portable.description, SCRAPE);
    assert.deepEqual(Object.keys(read('.claude-plugin', 'plugin.json')), ['name', 'version', 'description', 'homepage']);
    assert.equal(read('.codex-plugin', 'plugin.json').skills, './skills/');
    const skillText = readFileSync(path.join(root, 'skills', 'scraperight', 'SKILL.md'), 'utf8');
    assert.equal(skillText, scaffoldApi.buildCliSkillMarkdown('scraperight', 'scraperight', SCRAPE, ['scrape', 'crawl']));
    assert.match(skillText, /^---\nname: scraperight\ndescription: Scrape web pages/);
    assert.match(skillText, /`scraperight scrape`: run `scraperight scrape --help`/);
    assert.match(skillText, /command -v scraperight/);
    assert.doesNotMatch(skillText, /TODO|TBD|FIXME/);
    const clean = validateApi.validateRepoLayout(root);
    assert.deepEqual({ valid: clean.valid, layout: clean.layout, findings: clean.findings }, { valid: true, layout: 'portable', findings: [] });
    assert.equal(validateApi.detectLayout(root), 'portable');

    // An MCP command is optional on top; hooks add a manifest and a shim that validate clean.
    const withMcp = dir('scaffold-mcp');
    const mcp = scaffoldApi.scaffoldPlugin(withMcp, { cli: 'scraperight', skillDescription: SCRAPE, mcpCommand: 'scraperight serve --stdio' });
    assert.ok(mcp.mcpManifest && existsSync(path.join(withMcp, 'mcp.json')));
    assert.equal(validateApi.validateRepoLayout(withMcp).valid, true);
    const pushing = dir('scaffold-hooks');
    const hooked = scaffoldApi.scaffoldPlugin(pushing, { cli: 'pulse', skillDescription: PULSE, hookArgs: 'chat claude-hook', hookEvents: ['Stop'] });
    assert.ok(hooked.hooksManifest);
    assert.deepEqual(Object.keys(JSON.parse(readFileSync(path.join(pushing, 'hooks', 'hooks.json'), 'utf8')).hooks), ['Stop']);
    assert.ok(existsSync(path.join(pushing, 'hooks', 'run-pulse.sh')));
    const pushed = validateApi.validateRepoLayout(pushing);
    assert.deepEqual({ valid: pushed.valid, findings: pushed.findings }, { valid: true, findings: [] });
    assert.deepEqual(hooksApi.validateHooks(pushing), []);

    // Scaffolding refuses what the gates would reject, and writes nothing.
    const refused = dir('scaffold-refused');
    assert.throws(() => scaffoldApi.scaffoldPlugin(refused, { cli: 'scraperight', skillDescription: 'A widget tool.' }), /description gate/);
    assert.equal(existsSync(path.join(refused, 'plugin.json')), false);
    assert.throws(() => scaffoldApi.scaffoldPlugin(dir('s'), { cli: 'scraperight' }), /skill description/);
    assert.throws(() => scaffoldApi.scaffoldPlugin(dir('s'), { cli: '../scraperight', skillDescription: SCRAPE }), /bare program name/);
    assert.throws(() => scaffoldApi.scaffoldPlugin(dir('s'), { cli: 'scraperight', skillDescription: SCRAPE, skillName: '../x' }), /kebab-case/);
    assert.throws(() => scaffoldApi.scaffoldPlugin(dir('s'), { cli: 'pulse', skillDescription: PULSE, hookArgs: 'x', hookEvents: ['Nope'] }), /unknown hook event/);
    assert.throws(() => scaffoldApi.scaffoldPlugin(dir('s'), { hookArgs: 'chat claude-hook' }), /--cli/);

    // The pure builders.
    const pluginManifest = scaffoldApi.buildPluginManifest(dir('builder'), { name: 'widget', version: '1.2.3', description: SCRAPE });
    assert.equal(pluginManifest.name, 'widget');
    assert.equal(pluginManifest.version, '1.2.3');
    assert.equal(typeof scaffoldApi.buildSkillMarkdown, 'function');
    const mcpManifest = scaffoldApi.buildMcpManifest('node server.mjs');
    assert.equal(mcpManifest.$schema, 'https://agent-plugins.org/schemas/1.0.0/mcp.schema.json');
    assert.deepEqual(mcpManifest.mcpServers, { default: { type: 'stdio', command: 'node', args: ['server.mjs'] } });
    assert.equal(scaffoldApi.buildMcpManifest(''), null);

    // Validation findings: schema, unknown fields, path containment, layouts.
    const finding = (files, expect) => {
      const repo = dir('validate');
      for (const [rel, value] of Object.entries(files)) put(repo, rel, JSON.stringify(value));
      const outcome = validateApi.validateRepoLayout(repo);
      expect(outcome);
      return outcome;
    };
    const SCHEMA = 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json';
    const MCP_SCHEMA = 'https://agent-plugins.org/schemas/1.0.0/mcp.schema.json';
    finding({ 'plugin.json': { $schema: SCHEMA, name: 'bad--name' } }, outcome => { assert.equal(outcome.valid, false); assert.ok(outcome.findings.some(f => f.ruleId === 'plugin-json-schema' && /pattern mismatch/.test(f.evidence))); });
    finding({ 'plugin.json': { name: 'ok-name' } }, outcome => { assert.equal(outcome.valid, false); assert.ok(outcome.findings.some(f => /missing required property \$schema/.test(f.evidence))); });
    finding({ 'plugin.json': { $schema: SCHEMA, name: 'ok-name', unknownTopLevel: true } }, outcome => { assert.equal(outcome.valid, true); assert.ok(outcome.findings.some(f => f.severity === 'info' && /unexpected property unknownTopLevel/.test(f.evidence))); });
    const server = extra => ({ 'plugin.json': { $schema: SCHEMA, name: 'ok-name' }, 'mcp.json': { $schema: MCP_SCHEMA, mcpServers: { main: { type: 'stdio', command: 'node', ...extra } } } });
    finding(server({ args: ['server.mjs'], env: { DATA_DIR: '${PLUGIN_DATA}/cache' } }), outcome => assert.equal(outcome.valid, true, JSON.stringify(outcome.findings)));
    for (const [extra, evidence] of [[{ cwd: '${PLUGIN_ROOT}/../../etc' }, /./], [{ args: ['../../outside/server.mjs'] }, /traversal/], [{ args: ['/etc/passwd'] }, /absolute path/], [{ cwd: 'C:escape-from-root' }, /drive-relative/]]) {
      finding(server(extra), outcome => { assert.equal(outcome.valid, false); assert.ok(outcome.findings.some(f => f.ruleId === 'plugin-path-containment' && evidence.test(f.evidence)), JSON.stringify(outcome.findings)); });
    }
    assert.equal(validateApi.lintPluginMetadata({ name: 'widget', description: SCRAPE }, 'plugin.json').filter(f => f.severity === 'error').length, 0);
    const schemas = validateApi.loadPinnedSchemas();
    assert.equal(typeof schemas, 'object');
    // Host-native layouts as shipped by Pulse.
    const claudeNative = dir('claude-native');
    put(claudeNative, '.claude-plugin/plugin.json', JSON.stringify({ name: 'pulse', description: 'Pulse Chat for Claude Code.', version: '0.1.0', author: { name: 'Orthic Labs' } }));
    put(claudeNative, 'skills/pulse-bridge/SKILL.md', `---\nname: pulse-bridge\ndescription: ${PULSE}\n---\n\n# Pulse\n`);
    assert.equal(validateApi.detectLayout(claudeNative), 'claude-native');
    assert.equal(validateApi.validateRepoLayout(claudeNative).valid, true);

    // Hooks: the exact constants, the builder, and validation findings.
    assert.equal(hooksApi.HOOKS_FILE, 'hooks/hooks.json');
    assert.deepEqual([...hooksApi.DEFAULT_PUSH_EVENTS], ['UserPromptSubmit', 'PostToolUse', 'Stop']);
    for (const event of hooksApi.DEFAULT_PUSH_EVENTS) assert.ok(hooksApi.KNOWN_HOOK_EVENTS.includes(event), event);
    const built = hooksApi.buildHooksFiles('pulse', { hookArgs: 'chat claude-hook', events: ['Stop'] });
    assert.equal(built.shimName, 'run-pulse.sh');
    assert.deepEqual(Object.keys(built.manifest.hooks), ['Stop']);
    assert.equal(built.manifest.hooks.Stop[0].hooks[0].command, 'sh "${CLAUDE_PLUGIN_ROOT}/hooks/run-pulse.sh" chat claude-hook');
    assert.match(built.script, /^#!\/bin\/sh\n/);
    assert.match(built.script, /command -v pulse/);
    const handler = command => ({ hooks: { Stop: [{ hooks: [{ type: 'command', command, timeout: 10 }] }] } });
    const hookRoot = (hooks, script) => { const root = dir('hooks'); put(root, 'hooks/hooks.json', JSON.stringify(hooks)); if (script) put(root, 'hooks/run.sh', script); return root; };
    assert.deepEqual(hooksApi.validateHooks(dir('hooks-none')), [], 'no hooks file means no findings');
    assert.deepEqual(hooksApi.validateHooks(hookRoot(handler('sh "${CLAUDE_PLUGIN_ROOT}/hooks/run.sh" claude-hook'), '#!/bin/sh\ncommand -v tool >/dev/null 2>&1 || exit 0\nexec tool chat claude-hook\n')), []);
    assert.deepEqual(errorRules(hooksApi.validateHooks(hookRoot(handler('sh "${CLAUDE_PLUGIN_ROOT}/hooks/run.sh" claude-hook')))), ['hooks-script-missing']);
    assert.ok(errorRules(hooksApi.validateHooks(hookRoot(handler('sh "${CLAUDE_PLUGIN_ROOT}/../evil.sh"')))).includes('hooks-script-containment'));
    for (const command of ['/Users/someone/bin/tool hook', 'C:\\Tools\\tool.exe hook', '\\\\host\\share\\tool.exe hook', 'sh "/opt/tool/run.sh"']) {
      assert.ok(errorRules(hooksApi.validateHooks(hookRoot(handler(command)))).includes('hooks-absolute-path'), command);
    }
    const homePath = hooksApi.validateHooks(hookRoot(handler('sh "${CLAUDE_PLUGIN_ROOT}/hooks/run.sh" claude-hook'), '#!/bin/sh\nexec /Users/adrian/bin/tool\n'));
    assert.ok(homePath.some(f => f.ruleId === 'hooks-absolute-path' && /run\.sh/.test(f.evidence)));
    return { cliSkill: true, mcpOptional: true, hooks: true, findingsChecked: 9, refusals: 6, nativeLayout: true };
  });

  await run('portable-core-assembly-and-validation', async () => {
    assert.equal(portableApi.PORTABLE_CORE_SCHEMA_VERSION, 1);
    assert.equal(portableApi.PORTABLE_CORE_KIND, 'rightax-portable-core');
    assert.equal(typeof portableApi.ANTIGRAVITY_PLUGIN_SCHEMA, 'string');
    assert.ok(Object.isFrozen(portableApi.PORTABLE_CORE_SCHEMA), 'the contract schema is frozen');
    assert.deepEqual(portableApi.PORTABLE_CORE_SCHEMA.required, ['schemaVersion', 'kind', 'plugin', 'publicSkills', 'publicFiles', 'privateWorkspaceContent', 'clientProjections']);
    assert.deepEqual(portableApi.PORTABLE_CORE_SCHEMA.properties.kind, { const: portableApi.PORTABLE_CORE_KIND });
    assert.equal(portableApi.assemble, portableApi.assemblePortableCore);
    assert.equal(portableApi.validatePortableCoreDirectory, portableApi.validatePortableCore);
    const projections = () => Object.fromEntries(Object.entries(portableApi.CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration]));
    const fixture = () => {
      const root = dir('portable');
      const source = path.join(root, 'source');
      put(source, 'audit/SKILL.md', '# Audit\n');
      put(root, 'plugin.json', JSON.stringify({ name: 'legion', version: '0.1.0' }));
      return { root, source, output: path.join(root, 'output') };
    };
    const f = fixture();
    put(f.source, 'agents/sage.md', '# Sage\n');
    put(f.root, 'hooks.json', JSON.stringify({ description: 'guard', hooks: { SessionStart: [] } }));
    const contract = portableApi.assemblePortableCore({
      outputDir: f.output, pluginManifestPath: path.join(f.root, 'plugin.json'), hooksManifestPath: path.join(f.root, 'hooks.json'),
      skills: [{ id: 'audit', visibility: 'public', sourceRoot: f.source, sourceDir: path.join(f.source, 'audit') }],
      agents: [{ name: 'sage', sourceRoot: f.source, sourceFile: path.join(f.source, 'agents', 'sage.md') }],
      clientProjections: projections(),
    });
    assert.deepEqual(contract.publicSkills, ['audit']);
    assert.deepEqual(contract.publicAgents, ['sage']);
    for (const file of ['hooks/hooks.json', 'agents/sage.md']) assert.ok(contract.publicFiles.includes(file), file);
    assert.equal(contract.clientProjections.pi.executableRegistration, false);
    assert.equal(readFileSync(path.join(f.output, 'agents', 'sage.md'), 'utf8'), '# Sage\n');
    assert.deepEqual(portableApi.validatePortableCore(f.output).errors, []);
    const checked = portableApi.validatePortableCoreContract(contract);
    assert.deepEqual([checked.valid, checked.errors], [true, []]);
    assert.equal(portableApi.validatePortableCoreContract({ kind: 'other' }).valid, false);
    assert.equal(portableApi.validatePortableCore(contract).valid, true, 'a contract object is validated as a contract');
    assert.deepEqual(portableApi.validateResourceClosure(f.output), []);
    assert.deepEqual(Object.keys(portableApi.normalizeClientProjections(projections())).sort(), Object.keys(portableApi.CLIENT_PROJECTION_KINDS).sort());
    // Private or personal skills and a missing client boundary are refused.
    for (const declaration of [{ visibility: 'private' }, { visibility: 'public', personal: true }]) {
      const g = fixture();
      assert.throws(() => portableApi.assemblePortableCore({ outputDir: g.output, pluginManifestPath: path.join(g.root, 'plugin.json'), skills: [{ id: 'audit', sourceRoot: g.source, sourceDir: path.join(g.source, 'audit'), ...declaration }], clientProjections: {} }), /rejects/);
    }
    // A tampered output no longer validates.
    put(f.output, 'unlisted/extra.md', 'not declared\n');
    assert.ok(portableApi.validatePortableCore(f.output).errors.length > 0, 'a file the contract does not declare is detected');
    return { assembled: true, skills: 1, agents: 1, hooks: true, refusals: 2, tamperDetected: true };
  });

  await run('marketplace-source-list-build-and-validate', async () => {
    assert.equal(marketplaceApi.SOURCE_LIST_SCHEMA_VERSION, 1);
    assert.equal(marketplaceApi.CLAUDE_MARKETPLACE_PATH, '.claude-plugin/marketplace.json');
    assert.equal(marketplaceApi.CODEX_MARKETPLACE_PATH, '.agents/plugins/marketplace.json');
    assert.equal(marketplaceApi.CLAUDE_MARKETPLACE_SCHEMA, 'https://json.schemastore.org/claude-code-plugin-marketplace.json');
    assert.deepEqual([...marketplaceApi.HOSTS], ['claude', 'codex']);
    const sources = () => ({
      schemaVersion: 1, name: 'right-suite', owner: { name: 'Orthic Labs' },
      plugins: [
        { name: 'pulse', description: 'Message AI chats on paired machines through Pulse chat.', category: 'productivity', version: '0.1.0', url: 'https://example.invalid/pulse.git', ref: 'main', hosts: { claude: { path: 'plugins/claude/pulse' }, codex: { path: 'plugins/codex/pulse' } } },
        { name: 'legion', description: 'Legion orchestrates capability selection and authority attachment for AI-assisted engineering.', version: '0.3.23', url: 'https://example.invalid/legion.git', ref: 'main', hosts: { claude: { path: '.' } } },
      ],
    });
    assert.deepEqual(marketplaceApi.parseSourceList(sources()).findings, []);
    const refused = mutate => { const input = sources(); mutate(input.plugins[0], input); const parsed = marketplaceApi.parseSourceList(input); assert.equal(parsed.list, null); assert.ok(parsed.findings.some(item => item.severity === 'error')); };
    refused(entry => { entry.name = 'Pulse Chat'; });
    refused((entry, input) => { input.plugins[1].name = entry.name; });
    refused(entry => { entry.url = 'http://example.invalid/pulse.git'; });
    refused(entry => { entry.url = 'git@example.invalid:pulse.git'; });
    refused(entry => { entry.url = 'file:///tmp/pulse'; });
    refused(entry => { entry.url = 'https://user:secret@example.invalid/pulse.git'; });
    refused(entry => { delete entry.ref; });
    refused(entry => { entry.sha = 'abc123'; });
    refused(entry => { entry.version = 'one'; });
    refused(entry => { entry.hosts.claude.path = '../escape'; });
    refused(entry => { entry.hosts.claude.local = '/abs/plugin'; });
    refused(entry => { entry.hosts.claude.local = './a/../b'; });
    refused(entry => { entry.hosts.claude.local = 'plugins/pulse'; });
    refused((entry, input) => { input.schemaVersion = 2; });
    refused((entry, input) => { delete input.owner; });
    assert.equal(marketplaceApi.parseSourceList(null).list, null);
    const asFile = sources(); asFile.plugins[0].url = 'file:///tmp/pulse';
    assert.equal(marketplaceApi.parseSourceList(asFile).list, null);
    assert.ok(marketplaceApi.parseSourceList(asFile, { allowFileUrls: true }).list);

    // The generated manifests: git-subdir for a path in a repository, url for the repository root, local for a vendored plugin.
    const list = marketplaceApi.parseSourceList(sources()).list;
    const claude = marketplaceApi.buildClaudeMarketplace(list);
    assert.equal(claude.$schema, marketplaceApi.CLAUDE_MARKETPLACE_SCHEMA);
    assert.deepEqual(claude.plugins[0].source, { source: 'git-subdir', url: 'https://example.invalid/pulse.git', path: 'plugins/claude/pulse', ref: 'main' });
    assert.deepEqual(claude.plugins[1].source, { source: 'url', url: 'https://example.invalid/legion.git', ref: 'main' });
    const remoteCodex = marketplaceApi.buildCodexMarketplace(list);
    assert.deepEqual(remoteCodex.manifest.plugins, []);
    assert.deepEqual(remoteCodex.skipped, ['pulse']);
    const vendored = sources(); vendored.plugins[0].hosts = { claude: { local: './plugins/claude/pulse' }, codex: { local: './plugins/codex/pulse' } }; delete vendored.plugins[0].url; delete vendored.plugins[0].ref;
    const vendoredList = marketplaceApi.parseSourceList(vendored).list;
    assert.equal(marketplaceApi.buildClaudeMarketplace(vendoredList).plugins[0].source, './plugins/claude/pulse');
    assert.deepEqual(marketplaceApi.buildCodexMarketplace(vendoredList).manifest.plugins[0].source, { source: 'local', path: './plugins/codex/pulse' });
    assert.deepEqual(marketplaceApi.renderMarketplaceFiles(list).files.map(file => file.path), ['.claude-plugin/marketplace.json']);
    assert.deepEqual(marketplaceApi.renderMarketplaceFiles(vendoredList).files.map(file => file.path), ['.claude-plugin/marketplace.json', '.agents/plugins/marketplace.json']);

    // Build, check for drift, and validate a repository that vendors its plugins.
    const parent = dir('marketplace');
    const root = path.join(parent, 'right-plugins');
    const pulseJson = (host, text) => JSON.stringify({ name: 'pulse', version: '0.1.0', description: text });
    put(root, 'plugins/claude/pulse/.claude-plugin/plugin.json', JSON.stringify({ name: 'pulse', version: '0.1.0', description: 'Pulse Chat for Claude Code.', author: { name: 'Orthic Labs' } }));
    put(root, 'plugins/claude/pulse/skills/pulse-bridge/SKILL.md', `---\nname: pulse-bridge\ndescription: ${PULSE}\n---\n\n# Pulse\n`);
    put(root, 'plugins/codex/pulse/.codex-plugin/plugin.json', JSON.stringify({ name: 'pulse', version: '0.1.0', description: 'Pulse Chat for Codex.', skills: './skills/' }));
    put(root, 'plugins/codex/pulse/skills/pulse-bridge/SKILL.md', `---\nname: pulse-bridge\ndescription: ${PULSE}\n---\n\n# Pulse\n`);
    void pulseJson;
    const sourcesPath = put(root, 'marketplace/plugins.json', JSON.stringify({ ...vendored, plugins: [vendored.plugins[0]] }));
    assert.equal(marketplaceApi.readSourceList(sourcesPath).list.name, 'right-suite');
    const built = marketplaceApi.buildMarketplace({ sourcesPath, outDir: root });
    assert.equal(built.ok, true, JSON.stringify(built.findings));
    const generated = JSON.parse(readFileSync(path.join(root, '.claude-plugin', 'marketplace.json'), 'utf8'));
    assert.equal(generated.plugins[0].source, './plugins/claude/pulse');
    assert.equal(marketplaceApi.buildMarketplace({ sourcesPath, outDir: root, check: true }).ok, true);
    assert.equal(marketplaceApi.workspaceIsMarketplaceRoot(root, sourcesPath), true);
    assert.equal(marketplaceApi.workspaceIsMarketplaceRoot(path.join(parent, 'elsewhere'), sourcesPath), false);
    assert.equal(marketplaceApi.workspaceIsMarketplaceRoot(undefined, sourcesPath), false);
    const valid = marketplaceApi.validateMarketplace({ sourcesPath, outDir: root, workspace: root });
    assert.equal(valid.valid, true, JSON.stringify(valid, null, 2));
    assert.deepEqual(valid.plugins.map(record => [record.plugin, record.host]), [['pulse', 'claude'], ['pulse', 'codex']]);
    assert.match(valid.plugins[0].source, /^local \.\/plugins\/claude\/pulse in marketplace repository /);
    assert.equal(marketplaceApi.validateMarketplace({ sourcesPath, skipDrift: true }).valid, false, 'a local plugin needs the repository root');
    // A stale generated file is drift even though every plugin validates.
    writeFileSync(path.join(root, '.claude-plugin', 'marketplace.json'), '{}\n');
    assert.deepEqual(marketplaceApi.buildMarketplace({ sourcesPath, outDir: root, check: true }).drift, ['.claude-plugin/marketplace.json']);
    const stale = marketplaceApi.validateMarketplace({ sourcesPath, outDir: root, workspace: root });
    assert.equal(stale.valid, false);
    assert.ok(stale.findings.some(item => item.ruleId === 'marketplace-drift'));
    // A vague skill description fails the listed plugin, and a missing local path is unresolved.
    put(root, 'plugins/claude/pulse/skills/pulse-bridge/SKILL.md', '---\nname: pulse-bridge\ndescription: A chat tool.\n---\n');
    const vague = marketplaceApi.validateMarketplace({ sourcesPath, outDir: root, workspace: root, skipDrift: true });
    assert.equal(vague.valid, false);
    assert.ok(vague.plugins[0].findings.some(item => item.ruleId === 'skill-description-length'));
    const missing = sources(); missing.plugins = [missing.plugins[0]]; missing.plugins[0].hosts = { claude: { local: './plugins/nowhere' } }; delete missing.plugins[0].url; delete missing.plugins[0].ref;
    const missingPath = put(root, 'marketplace/missing.json', JSON.stringify(missing));
    const gone = marketplaceApi.validateMarketplace({ sourcesPath: missingPath, outDir: root, skipDrift: true });
    assert.ok(gone.plugins[0].findings.some(item => item.ruleId === 'marketplace-plugin-unresolved'));
    assert.equal(marketplaceApi.buildMarketplace({ sourcesPath: path.join(root, 'marketplace', 'none.json'), outDir: root }).ok, false);
    return { sourceListRefusals: 15, manifestsBuilt: 2, driftDetected: true, vendoredValidation: true };
  });

  await run('static-contract-rules-over-an-mcp-surface', async () => {
    const outputSchema = { type: 'object', properties: { status: { type: 'string' }, truncated: { type: 'boolean' }, continuationCursor: { type: ['string', 'null'] }, claimBoundary: { type: 'object', properties: { status: {}, cleanClaimAllowed: {}, safeClaims: {}, prohibitedClaims: {}, gaps: {} }, required: ['status', 'cleanClaimAllowed', 'safeClaims', 'prohibitedClaims', 'gaps'] } } };
    const goodTools = () => ['a', 'b'].map(letter => ({ name: `widget_${letter}`, description: `Use this when you need widget ${letter.toUpperCase()}. Do not use it for widget ${letter === 'a' ? 'B' : 'A'}.`, inputSchema: { type: 'object', additionalProperties: false, properties: { query: { type: 'string' } } }, outputSchema, annotations: { readOnlyHint: true } }));
    const goodRepo = () => {
      const repo = dir('static-good');
      put(repo, 'plugin.json', JSON.stringify({ $schema: 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json', name: 'widget' }));
      put(repo, 'mcp/effects.mjs', 'export const TOOL_EFFECTS = { widget_a: { reads: [], writes: [], executesProjectCode: false, network: "none", installsSoftware: false, destructive: false, idempotent: true, approval: "not_required" }, widget_b: {} };\nexport function failureEnvelope(code, message) { return { error: { code, message, retryable: false, remediation: "retry", redact: true } }; }\n');
      put(repo, 'skills/widget/SKILL.md', '# widget\n\nUses `widget_a` and `widget_b`. See `mcp/effects.mjs`.\n');
      return repo;
    };
    const passing = await staticApi.runStaticChecks({ repoPath: goodRepo(), manifest: { ax: { claimBoundaryRequired: true } }, tools: goodTools() });
    assert.deepEqual(passing.rules.filter(rule => rule.status === 'fail'), []);
    assert.equal(passing.verdict, 'pass');
    assert.equal(passing.gate, 'static');
    assert.equal(passing.toolCount, 2);
    assert.equal(new Set(passing.rules.map(rule => rule.ruleId.split('-').slice(0, 2).join('-'))).size, 14, 'fourteen rules ran');
    for (const rule of passing.rules) assert.deepEqual(Object.keys(rule).sort(), ['claim', 'evidence', 'remediation', 'ruleId', 'severity', 'status']);
    const failingRules = (mutate, repo = goodRepo()) => { const tools = goodTools(); mutate(tools); return staticApi.runStaticChecks({ repoPath: repo, manifest: {}, tools }).then(outcome => outcome.rules.filter(rule => rule.status === 'fail').map(rule => rule.ruleId)); };
    assert.ok((await failingRules(tools => { tools[1].description = tools[0].description; })).includes('static-1-tool-set'));
    assert.ok((await failingRules(tools => { tools[0].description = 'Does widget A things.'; })).includes('static-2-wire-description'));
    assert.ok((await failingRules(tools => { tools[0].inputSchema.additionalProperties = true; })).includes('static-3-strict-input-schema'));
    const bad = await staticApi.runStaticChecks({ repoPath: dir('static-bad'), manifest: {}, tools: [{ name: 'widget_a', description: 'Does a thing.', inputSchema: { type: 'object', additionalProperties: true, properties: { repoRoot: { type: 'string' } } } }] });
    assert.equal(bad.verdict, 'fail');
    assert.ok(bad.rules.filter(rule => rule.status === 'fail').length >= 3);
    // The live path: an MCP surface needs an artifact root outside the repository, and the server's tools are listed over stdio.
    const live = await staticApi.runStaticChecks({ repoPath: goodRepo(), manifest: mcpManifest(), artifactRoot });
    assert.equal(live.toolCount, 2);
    assert.equal(live.fatal, null);
    assert.equal(live.artifact.proof.immutable, true);
    const noRoot = await staticApi.runStaticChecks({ repoPath: goodRepo(), manifest: mcpManifest() });
    assert.equal(noRoot.fatal.code, 'invalid_artifact');
    assert.equal(noRoot.verdict, 'fail');
    const listed = await staticApi.fetchLiveTools({ command: 'node', args: ['fixture-mcp-server.mjs'], cwd: artifactRoot, timeoutMs: 30_000 });
    assert.deepEqual(listed.map(tool => tool.name), ['good_tool', 'bad_tool']);
    // The token grep skips generated and vendor directories.
    const grep = dir('grep');
    put(grep, 'src/a.mjs', 'export const alpha = 1; // beta gamma\n');
    put(grep, 'src/b.txt', 'alpha beta\n');
    put(grep, 'node_modules/x/c.mjs', 'alpha beta\n');
    assert.deepEqual(staticApi.grepRepoForTokens(grep, [['alpha', 'beta'], ['alpha', 'missing']]), [{ path: path.join('src', 'a.mjs'), tokens: ['alpha', 'beta'] }]);
    evidence.staticPassing = passing;
    evidence.staticFailing = bad;
    return { rules: passing.rules.length, passing: true, failingFixture: true, liveTools: listed.length, artifactImmutable: true };
  });

  await run('conformance-probes-against-a-live-mcp-server', async () => {
    const repo = dir('conformance-repo');
    const report = await conformanceApi.runConformance({ repoPath: repo, manifest: mcpManifest(), artifactRoot });
    assert.equal(report.fatal ?? null, null, JSON.stringify(report.fatal));
    assert.equal(report.gate, 'conformance');
    assert.equal(report.initialize.status, 'pass');
    const good = report.rows.find(row => row.tool === 'good_tool');
    const bad = report.rows.find(row => row.tool === 'bad_tool');
    assert.equal(good.status, 'pass', JSON.stringify(good.reasons));
    assert.deepEqual(good.probes.map(probe => [probe.kind, probe.status]), [['valid-args', 'pass'], ['missing-required', 'pass'], ['unknown-field', 'pass']]);
    assert.equal(good.checks.structuredContentValidation.status, 'pass');
    assert.equal(bad.status, 'fail');
    assert.ok(bad.reasons.some(reason => reason.includes('missing required arguments')));
    assert.equal(bad.checks.structuredContentValidation.status, 'skipped-pending');
    assert.equal(report.verdict, 'fail');
    assert.deepEqual(report.rawStackTraces, []);
    assert.equal(report.artifact.proof.immutable, true);
    assert.match(report.artifact.digest, /^[a-f0-9]{64}$/);
    // A declared example replaces the synthesized valid arguments.
    const withExample = await conformanceApi.runConformance({ repoPath: repo, manifest: mcpManifest({ conformanceExamples: { good_tool: { input: 'example', unexpected: true } } }), artifactRoot });
    const valid = withExample.rows.find(row => row.tool === 'good_tool').probes.find(probe => probe.kind === 'valid-args');
    assert.equal(valid.status, 'fail');
    assert.equal(valid.code, 'unknown_field');
    // Typed fatals: no MCP surface, and a server command that resolves outside the artifact root.
    assert.equal((await conformanceApi.runConformance({ repoPath: repo, manifest: {} })).fatal.code, 'no_mcp_surface');
    const outside = await conformanceApi.runConformance({ repoPath: repo, manifest: { surfaces: { mcp: { command: 'node', args: [path.join(os.tmpdir(), 'outside-server.mjs')], cwd: '${ARTIFACT_ROOT}' } } }, artifactRoot });
    assert.equal(outside.fatal?.code, 'invalid_artifact', JSON.stringify(outside.fatal));
    assert.equal(outside.verdict, 'fail');
    const inRepo = await conformanceApi.runConformance({ repoPath: repo, manifest: mcpManifest(), artifactRoot: repo });
    assert.equal(inRepo.fatal?.code, 'invalid_artifact', 'an artifact root inside the repository under test is refused');
    // The pure helpers.
    assert.deepEqual(conformanceApi.buildValidArgs({ inputSchema: { type: 'object', required: ['caller', 'mode', 'fixed', 'choice', 'alternate', 'attempts', 'items'], properties: {
      caller: { type: 'object', required: ['root', 'repositoryId'], properties: { root: { type: 'string', minLength: 2 }, repositoryId: { type: 'string' } } },
      mode: { enum: ['repo', 'workspace'] }, fixed: { const: 'fixed-value' }, choice: { oneOf: [{ type: 'string', minLength: 3 }, { type: 'integer', minimum: 5, maximum: 7 }] },
      alternate: { anyOf: [{ type: 'boolean' }, { type: 'string' }] }, attempts: { type: 'integer', minimum: 3, maximum: 7 },
      items: { type: 'array', minItems: 2, items: { type: 'object', required: ['name'], properties: { name: { type: 'string', maxLength: 4 } } } },
    } } }), { caller: { root: 'conformance-probe', repositoryId: 'conformance-probe' }, mode: 'repo', fixed: 'fixed-value', choice: 'conformance-probe', alternate: true, attempts: 3, items: [{ name: 'conf' }, { name: 'conf' }] });
    const example = { caller: { root: '/repo' } };
    const copy = conformanceApi.buildValidArgs({ inputSchema: { type: 'object', required: ['caller'], properties: { caller: { type: 'object' } } } }, example);
    assert.deepEqual(copy, example);
    copy.caller.root = '/changed';
    assert.equal(example.caller.root, '/repo', 'a declared example is deep-cloned');
    assert.deepEqual(conformanceApi.tryParseJson('{"a":1}'), { a: 1 });
    assert.equal(conformanceApi.tryParseJson('not json'), null);
    const tool = { inputSchema: { type: 'object', required: ['input'], properties: { input: { type: 'string' } } } };
    assert.equal(conformanceApi.evaluateProbe('valid-args', { kind: 'success' }, tool).status, 'pass');
    assert.equal(conformanceApi.evaluateProbe('valid-args', { kind: 'typed-error', code: 'x' }, tool).status, 'fail');
    assert.equal(conformanceApi.evaluateProbe('valid-args', { kind: 'stack-trace' }, tool).status, 'fail');
    assert.equal(conformanceApi.evaluateProbe('missing-required', { kind: 'typed-error', code: 'missing' }, tool).status, 'pass');
    assert.equal(conformanceApi.evaluateProbe('missing-required', { kind: 'success' }, tool).status, 'fail');
    assert.equal(conformanceApi.evaluateProbe('missing-required', { kind: 'success' }, { inputSchema: { type: 'object' } }).status, 'skipped-pending');
    // The blocking-capability probe fails closed without a harness binding.
    let calls = 0;
    const blocked = await conformanceApi.runBlockingCapabilityProbe({ bindingId: 'codex-windows', invoke: async () => { calls += 1; return { blocked: true, reason: 'denied', interceptionOrder: ['arcane'] }; } });
    assert.equal(calls, 1);
    assert.equal(blocked.verdict, 'pass');
    assert.deepEqual(blocked.observed, { blocked: true, reason: 'denied', interceptionOrder: ['arcane'] });
    const unbound = await conformanceApi.runBlockingCapabilityProbe({ bindingId: 'future-harness' });
    assert.equal(unbound.verdict, 'fail');
    assert.equal(unbound.observed.blocked, false);
    evidence.conformance = report;
    return { tools: report.rows.length, goodTool: 'pass', badTool: 'fail', fatals: 3, artifactBound: true, helpers: 5 };
  });

  await run('behavioral-scenarios-with-stub-and-declared-matrix', async () => {
    const scenarios = dir('scenarios');
    const scenario = (name, overrides = {}) => put(scenarios, `${name}.yaml`, JSON.stringify({
      scenarioId: name, goal: 'exercise the stub driver', allowedOperations: ['tool_a'], forbiddenOperations: ['tool_b'],
      expected: { finalEnvironment: { toolCalls: 1 }, requiredClaims: ['done'], forbiddenClaims: ['oops'] },
      fixture: { stub: { trial: { operations: ['tool_a'], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: 'done' } } },
      ...overrides,
    }));
    scenario('happy-path');
    put(scenarios, '_ignored.yaml', 'not even json');
    put(scenarios, 'notes.txt', 'ignored');
    assert.deepEqual(behavioralApi.listScenarioFiles(scenarios), ['happy-path.yaml']);
    assert.deepEqual(behavioralApi.listScenarioFiles(path.join(scenarios, 'missing')), []);
    const loaded = behavioralApi.loadScenarios(scenarios);
    assert.deepEqual(loaded.loadErrors, []);
    assert.equal(loaded.scenarios[0].scenarioId, 'happy-path');
    assert.equal(loaded.scenarios[0]._source, 'happy-path.yaml');

    // The environment is verified before any claim is admitted.
    const mismatch = behavioralApi.evaluateTrial({ expected: { finalEnvironment: { toolCalls: 1 }, requiredClaims: ['done'] } }, { finalEnvironment: { toolCalls: 2 }, answer: 'done', operations: [], reasonCodes: [] });
    assert.equal(mismatch.status, 'fail');
    assert.deepEqual([mismatch.envVerified, mismatch.claimsChecked], [false, false]);
    assert.ok(mismatch.failures[0].includes('verified before claims'));
    const claims = behavioralApi.evaluateTrial({ expected: { finalEnvironment: {}, requiredClaims: ['done'], forbiddenClaims: ['oops'], requiredReasonCodes: ['R1'], forbiddenReasonCodes: ['R2'] }, budgets: { maxToolCalls: 1 } }, { finalEnvironment: {}, answer: 'oops', operations: ['a', 'b'], reasonCodes: ['R2'] });
    assert.equal(claims.status, 'fail');
    for (const expected of ['required claim missing', 'forbidden claim present', 'required reason code missing', 'forbidden reason code present', 'tool call budget exceeded']) assert.ok(claims.failures.some(failure => failure.includes(expected)), expected);
    assert.deepEqual([claims.envVerified, claims.claimsChecked], [true, true]);
    assert.deepEqual(behavioralApi.stubTrial({ fixture: { stub: { trial: { operations: ['x'], answer: 7, contextTokens: 10 } } } }), { driver: 'stub', operations: ['x'], reasonCodes: [], finalEnvironment: {}, answer: '7', contextTokens: 10, invalidCalls: 0, durationMs: 48 });
    assert.equal(behavioralApi.stubTrial({}), null);

    const stub = await behavioralApi.runScenarios({ scenariosDir: scenarios, k: 3, driver: 'stub' });
    assert.equal(stub.fatal, null);
    assert.equal(stub.verdict, 'pass');
    assert.equal(stub.provenBy, 'stub');
    assert.equal(stub.scenarioResults['happy-path'].passCount, 3);
    assert.equal(stub.totals.pass, 3);
    // A real matrix needs at least two declared agents with at least five trials each.
    const matrixDir = dir('matrix-scenarios');
    put(matrixDir, "happy-path.yaml", readFileSync(path.join(scenarios, "happy-path.yaml"), "utf8"));
    let call = 0;
    const trialFor = agent => ({ operations: ['tool_a'], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: 'done', agentId: agent });
    const real = await behavioralApi.runScenarios({ scenariosDir: matrixDir, k: 10, driver: 'multi-agent', agents: ['agent-a', 'agent-b'], runTrial: async () => trialFor((call += 1) % 2 === 0 ? 'agent-a' : 'agent-b') });
    assert.equal(real.provenBy, 'real-matrix');
    assert.deepEqual(real.scenarioResults['happy-path'].agentTrialCounts, { 'agent-a': 5, 'agent-b': 5 });
    call = 0;
    const undeclared = await behavioralApi.runScenarios({ scenariosDir: matrixDir, k: 10, driver: 'multi-agent', runTrial: async () => trialFor((call += 1) % 2 === 0 ? 'agent-a' : 'agent-b') });
    assert.equal(undeclared.provenBy, 'stub', 'an undeclared roster is not a matrix');
    call = 0;
    const uneven = await behavioralApi.runScenarios({ scenariosDir: matrixDir, k: 5, driver: 'multi-agent', agents: ['agent-a', 'agent-b'], runTrial: async () => trialFor((call += 1) % 2 === 0 ? 'agent-a' : 'agent-b') });
    assert.equal(uneven.provenBy, 'stub', 'a 3 and 2 split is not a matrix');
    const failing = await behavioralApi.runScenarios({ scenariosDir: matrixDir, k: 2, driver: 'custom', runTrial: async () => ({ ...trialFor('solo'), answer: 'oops' }) });
    assert.equal(failing.verdict, 'fail');
    assert.equal(failing.scenarioResults['happy-path'].status, 'fail');
    // Fatals: no scenarios, and a scenario file that does not parse.
    assert.equal((await behavioralApi.runScenarios({ scenariosDir: dir('empty-scenarios'), k: 3 })).fatal.code, 'no_scenarios');
    const broken = dir('broken-scenarios');
    put(broken, 'bad.yaml', '{ nope');
    const brokenRun = await behavioralApi.runScenarios({ scenariosDir: broken, k: 3 });
    assert.equal(brokenRun.fatal.code, 'scenario_load_errors');
    assert.equal(brokenRun.verdict, 'fail');
    // The external-agent matrix refuses an invalid configuration before it starts any agent.
    const refusedMatrix = await behavioralApi.runMatrix({ repoPath: dir('matrix-repo'), artifactRoot, scenariosDir: matrixDir, matrix: { adapters: [], trialsPerAgent: 5, limits: {} } });
    assert.equal(refusedMatrix.fatal.code, 'matrix_invalid');
    assert.equal(refusedMatrix.verdict, 'fail');
    evidence.behavioral = stub;
    return { stubProvenBy: 'stub', realMatrix: true, unevenRefused: true, fatals: 3, claimsAndBudgets: 5 };
  });

  await run('adversarial-cases-canaries-and-live-session', async () => {
    const cases = items => { const casesDir = dir('cases'); items.forEach((item, index) => put(casesDir, `${index}.json`, JSON.stringify(item))); return casesDir; };
    const base = { caseId: 'secret-egress', tool: 'x', arguments: { q: 'CANARY', nested: [{ r: '${CANARY}' }] }, expect: { status: 'ok' } };
    let seen; let closed = 0;
    const clean = await adversarialApi.runAdversarial({ casesDir: cases([base]), artifactDigest: 'digest-1', invoke: async request => { seen = request; return { status: 'ok' }; }, close: async () => { closed += 1; } });
    assert.equal(clean.verdict, 'pass');
    assert.equal(clean.artifactDigest, 'digest-1');
    assert.deepEqual(clean.cases, [{ caseId: 'secret-egress', status: 'pass' }]);
    assert.match(seen.arguments.q, /RIGHTAX_CANARY_/);
    assert.equal(seen.arguments.nested[0].r, seen.arguments.q, 'every canary leaf is replaced by the same marker');
    assert.equal(closed, 1);
    const leaks = async response => (await adversarialApi.runAdversarial({ casesDir: cases([base]), artifactDigest: 'digest-1', invoke: async () => response })).verdict;
    assert.equal(await adversarialApi.runAdversarial({ casesDir: cases([base]), artifactDigest: 'd', invoke: async request => ({ status: 'ok', message: request.arguments.q }) }).then(r => r.verdict), 'fail', 'an echoed canary fails');
    assert.equal(await leaks({ status: 'ok', message: 'Error:\n at x (y)' }), 'fail', 'a stack trace fails');
    assert.equal(await leaks({ status: 'ok', message: 'C:\\secret' }), 'fail', 'a machine path fails');
    assert.equal(await leaks({ status: 'ok', message: 'x'.repeat(1024 * 1024 + 1) }), 'fail', 'an oversized response fails');
    assert.equal(await leaks({ status: 'denied' }), 'fail', 'an unexpected status fails');
    assert.equal(await adversarialApi.runAdversarial({ casesDir: cases([base]), artifactDigest: 'd', invoke: async () => { throw new Error('x'); } }).then(r => r.verdict), 'fail');
    // Malformed, duplicate and zero cases, and a missing digest, are typed fatals.
    assert.equal((await adversarialApi.runAdversarial({ casesDir: cases([]), artifactDigest: 'd', invoke: async () => ({ status: 'ok' }) })).fatal.code, 'invalid_cases');
    assert.equal((await adversarialApi.runAdversarial({ casesDir: cases([base, { ...base }]), artifactDigest: 'd', invoke: async () => ({ status: 'ok' }) })).fatal.code, 'invalid_cases');
    assert.equal((await adversarialApi.runAdversarial({ casesDir: cases([base]), invoke: async () => ({ status: 'ok' }) })).fatal.code, 'artifact_digest_required');
    for (const code of ['initialize_failed', 'tools_list_failed', 'session_open_failed']) {
      let closes = 0;
      const outcome = await adversarialApi.runAdversarial({ sessionFactory: async () => ({ fatal: { code }, close: async () => { closes += 1; } }) });
      assert.equal(outcome.fatal.code, code);
      assert.equal(closes, 1, 'a failed session is closed');
    }
    // A live session against the fixture server, bound to the artifact digest.
    const repo = dir('adversarial-repo');
    const manifest = mcpManifest();
    const session = await adversarialApi.openAdversarialSession({ repoPath: repo, manifest, artifactRoot });
    assert.equal(session.fatal, undefined);
    assert.deepEqual(session.tools.map(tool => tool.name), ['good_tool', 'bad_tool']);
    await session.close();
    assert.equal((await adversarialApi.openAdversarialSession({ repoPath: repo, manifest: {}, artifactRoot })).fatal.code, 'no_mcp_surface');
    const live = await adversarialApi.runAdversarial({ casesDir: cases([{ caseId: 'good', tool: 'good_tool', arguments: { input: 'CANARY' }, expect: { status: 'ok' } }]), repoPath: repo, manifest, artifactRoot });
    assert.equal(live.verdict, 'pass', JSON.stringify(live));
    assert.equal(live.artifactDigest, evidence.conformance.artifact.digest, 'the same artifact bytes are bound in every gate');
    const unadvertised = await adversarialApi.runAdversarial({ casesDir: cases([{ caseId: 'missing', tool: 'missing_tool', expect: { status: 'ok' } }]), repoPath: repo, manifest, artifactRoot });
    assert.equal(unadvertised.verdict, 'fail');
    assert.deepEqual(unadvertised.cases, [{ caseId: 'missing', status: 'fail' }]);
    evidence.adversarial = live;
    return { canaryReplaced: true, leakChecks: 6, fatals: 6, liveSession: true, digestBound: true };
  });

  await run('report-aggregation-sarif-and-exit-code', async () => {
    const manifest = { ax: { gates: { static: 'hard', conformance: 'hard', behavioral: 'report-only', adversarial: 'report-only' } } };
    const staticResult = evidence.staticPassing;
    const conformanceFailing = evidence.conformance;
    const report = reportApi.aggregateReport({ manifest, staticResult, conformanceResult: conformanceFailing, behavioralResult: evidence.behavioral, adversarialResult: evidence.adversarial });
    assert.equal(report.schemaVersion, 1);
    assert.equal(report.gateResult, 'fail', 'a failing conformance row is never masked');
    assert.equal(report.signoff, null);
    assert.equal(reportApi.gateExitCode(report), 1);
    assert.equal(report.gates.static.status, 'pass');
    assert.equal(report.gates.conformance.status, 'fail');
    assert.equal(report.gates.behavioral.status, 'report-only');
    assert.equal(report.gates.behavioral.provenBy, 'stub');
    assert.match(report.gates.behavioral.claim, /UNPROVEN/);
    assert.equal(report.gates.adversarial.caseCount, 1);
    assert.equal(report.executedGateCount, 4);
    assert.equal(report.fatal, null, JSON.stringify(report.fatal));
    assert.equal(report.digests.artifact, conformanceFailing.artifact.digest);
    assert.equal(report.digests.adversarial, evidence.adversarial.artifactDigest);
    const markdown = reportApi.renderMarkdown(report);
    assert.match(markdown, /^# right-ax scorecard/);
    assert.match(markdown, /Gate result: \*\*FAIL\*\*/);
    assert.match(markdown, /never signs off/);
    assert.match(markdown, /## UNPROVEN/);
    assert.doesNotMatch(markdown, /\bclean\b/i);
    const sarif = reportApi.toSarif({ staticResult: evidence.staticFailing, conformanceResult: conformanceFailing, behavioralResult: null, adversarialResult: null });
    assert.equal(sarif.version, '2.1.0');
    assert.equal(sarif.runs[0].tool.driver.name, 'right-ax');
    assert.ok(sarif.runs[0].results.length > 0);
    assert.ok(sarif.runs[0].results.every(item => item.level && item.message.text && item.ruleId));
    assert.equal(reportApi.toSarif({ staticResult }).runs[0].results.length, 0, 'a passing gate has no results');
    // Only passing gates pass; a report-only gate that fails is still a failure; a forged verdict cannot hide a leaf failure.
    const passing = reportApi.aggregateReport({ manifest, staticResult, conformanceResult: { rows: [], totals: {}, verdict: 'pass', artifact: { digest: 'one' } }, behavioralResult: { provenBy: 'stub', scenarioCount: 1, totals: { pass: 3, fail: 0 }, verdict: 'pass' }, adversarialResult: null });
    assert.equal(passing.gateResult, 'pass');
    assert.equal(reportApi.gateExitCode(passing), 0);
    assert.match(reportApi.renderMarkdown(passing), /Gate result: \*\*PASS\*\*/);
    const reportOnly = reportApi.aggregateReport({ manifest: { ax: { gates: { static: 'report-only', conformance: 'report-only', behavioral: 'report-only' } } }, staticResult: evidence.staticFailing, conformanceResult: { rows: [], totals: {}, verdict: 'pass', artifact: { digest: 'one' } }, behavioralResult: null });
    assert.equal(reportOnly.gateResult, 'fail');
    assert.equal(reportOnly.gates.static.status, 'report-only-failing');
    assert.doesNotMatch(reportApi.renderMarkdown(reportOnly), /Gate result: \*\*PASS\*\*/);
    const forged = reportApi.aggregateReport({ manifest, staticResult: { ...evidence.staticFailing, verdict: 'pass' }, conformanceResult: { rows: [], verdict: 'pass', artifact: { digest: 'one' } } });
    assert.equal(forged.gateResult, 'fail');
    // Gates that ran against different artifact bytes, or that did not run at all, are fatal.
    const mismatched = reportApi.aggregateReport({ manifest, staticResult, conformanceResult: { rows: [], verdict: 'pass', artifact: { digest: 'one' } }, adversarialResult: { verdict: 'pass', artifactDigest: 'two', cases: [] } });
    assert.equal(mismatched.gateResult, 'fail');
    assert.ok(mismatched.fatal);
    const nothing = reportApi.aggregateReport({ manifest });
    assert.equal(nothing.gateResult, 'fail');
    assert.ok(nothing.fatal);
    assert.equal(nothing.executedGateCount, 0);
    return { gateResult: 'fail', unprovenBehavioral: true, sarifResults: sarif.runs[0].results.length, exitCodes: [1, 0], fatals: 2 };
  });

  await run('ownership-review-packet-and-receipt-validation', async () => {
    // Git is the one external program this scenario needs; it is run with no user or system configuration.
    process.env.GIT_CONFIG_NOSYSTEM = '1';
    const git = (cwd, args) => { const outcome = spawnSync('git', ['-c', 'core.autocrlf=false', '-c', 'commit.gpgsign=false', '-c', 'user.email=ax@example.invalid', '-c', 'user.name=AX consumer', ...args], { cwd, encoding: 'utf8', windowsHide: true }); assert.equal(outcome.status, 0, outcome.stderr); return outcome.stdout.trim(); };
    const root = dir('ownership');
    git(root, ['init', '-q']);
    put(root, 'rightkit/packages/release/rightkit-ownership.json', JSON.stringify({ schema: 1, capabilities: [{ package: 'rightkit-oauth', capability: 'oauth-sign-in', summary: 'OAuth flow', fingerprints: [{ id: 'oauth.source.flow', type: 'source', pattern: 'device_flow' }] }] }));
    put(root, 'rightkit/packages/oauth/README.md', '# OAuth API\nUse shared device-flow helper.\n');
    put(root, 'rightkit/packages/oauth/src/signin.mjs', 'fn login() { shared_oauth(); }\n');
    git(root, ['add', '.']);
    git(root, ['commit', '-qm', 'base']);
    const base = git(root, ['rev-parse', 'HEAD']);
    put(root, 'rightkit/packages/oauth/src/signin.mjs', 'fn login() { local_device_flow(); }\n');
    git(root, ['add', '.']);
    git(root, ['commit', '-qm', 'head']);
    const head = git(root, ['rev-parse', 'HEAD']);
    const limits = ownershipApi.OWNERSHIP_REVIEW_LIMITS;
    assert.deepEqual(limits, { maxDiffBytes: 180 * 1024, maxChangedFiles: 100, maxCatalogBytes: 96 * 1024, maxApiBytes: 48 * 1024, maxOutputBytes: 24 * 1024, maxCostUsd: 1.5 });
    assert.ok(Object.isFrozen(limits));
    const packet = ownershipApi.buildOwnershipReviewPacket({ repoPath: root, base, head });
    assert.equal(packet.state, 'prepared');
    assert.equal(packet.advisoryOnly, true);
    assert.equal(packet.authority.mayApproveExceptions, false);
    assert.equal(packet.authority.mayBlockChanges, false);
    assert.deepEqual(packet.range, { base, head });
    assert.deepEqual(packet.changedFiles, ['rightkit/packages/oauth/src/signin.mjs']);
    assert.match(packet.source.diff.content, /local_device_flow/);
    assert.equal(packet.source.apiSources[0].path, 'rightkit/packages/oauth/README.md');
    assert.match(packet.requestId, /^[a-f0-9]{64}$/);
    assert.equal(packet.providerState.status, 'not_started');
    assert.deepEqual(packet.limits, limits);
    assert.throws(() => ownershipApi.buildOwnershipReviewPacket({ repoPath: root, base, head, limits: { maxDiffBytes: 8 } }), /split review/);
    assert.throws(() => ownershipApi.buildOwnershipReviewPacket({ repoPath: root, base, head, limits: { maxChangedFiles: 101 } }), /over-policy limit/);
    assert.throws(() => ownershipApi.buildOwnershipReviewPacket({ repoPath: root, base: '--upload-pack=x', head }), /commit refs/);
    // The packet is written exclusively, read back, and checked for integrity.
    const file = path.join(root, '..', `packet-${counter}.json`);
    ownershipApi.writeOwnershipReviewPacket(file, packet);
    assert.throws(() => ownershipApi.writeOwnershipReviewPacket(file, packet), /EEXIST|exist/i, 'an existing packet is never overwritten');
    const reread = ownershipApi.readOwnershipReviewJson(file);
    assert.deepEqual(reread, packet);
    assert.equal(ownershipApi.assertOwnershipReviewPacket(reread), reread);
    assert.throws(() => ownershipApi.assertOwnershipReviewPacket({ ...reread, source: { ...reread.source, diff: { ...reread.source.diff, content: 'altered' } } }), /integrity failed/);
    assert.throws(() => ownershipApi.assertOwnershipReviewPacket({ kind: 'other' }), /invalid prepared/);
    // A receipt must carry supervised evidence and ground every finding in the packet.
    const receipt = (overrides = {}) => ({
      requestId: packet.requestId, inputHash: packet.inputHash, usage: { costUsd: 0.12 },
      supervision: { identity: { verified: true, sessionId: 'session-1', backend: 'claude', tools: 'read' }, nativeReceiptDigest: 'c'.repeat(64), transcript: { verified: true, digest: 'a'.repeat(64) }, controlWrite: { verified: true }, reconnect: { verified: true }, completion: { verified: true, status: 'completed' } },
      review: { findings: [{ file: 'rightkit/packages/oauth/src/signin.mjs', line: 1, catalogRefs: ['oauth.source.flow'], rationale: 'Local flow appears to overlap shared capability.', confidence: 0.78 }] },
      ...overrides,
    });
    const accepted = ownershipApi.validateOwnershipReviewReceipt(packet, receipt());
    assert.equal(accepted.valid, true);
    assert.equal(accepted.state, 'completed');
    assert.equal(accepted.advisoryOnly, true);
    assert.equal(accepted.findings.length, 1);
    assert.deepEqual(accepted.provider, { backend: 'claude', tools: 'read', sessionId: 'session-1' });
    const incomplete = ownershipApi.validateOwnershipReviewReceipt(packet, receipt({ supervision: { identity: { verified: true } } }));
    assert.equal(incomplete.valid, false);
    assert.equal(incomplete.state, 'receipt_incomplete');
    assert.equal(ownershipApi.validateOwnershipReviewReceipt(packet, receipt({ review: { findings: [{ file: 'outside.rs', line: 3, catalogRefs: ['made.up'], rationale: 'Concern.', confidence: 0.5 }] } })).valid, false);
    assert.equal(ownershipApi.validateOwnershipReviewReceipt(packet, receipt({ usage: { costUsd: 99 } })).valid, false, 'a cost over the limit is refused');
    assert.equal(ownershipApi.validateOwnershipReviewReceipt(packet, receipt({ requestId: 'f'.repeat(64) })).valid, false, 'a receipt for another request is refused');
    assert.match(ownershipApi.validateOwnershipReviewReceipt({ ...packet, source: { ...packet.source, diff: { ...packet.source.diff, content: 'altered' } } }, {}).errors.join(' '), /diff content digest mismatch/);
    return { packetBounded: true, advisoryOnly: true, integrity: true, receiptsChecked: 6 };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch (error) {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
} finally {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
}
process.stdout.write(`${JSON.stringify(result)}\n`);
