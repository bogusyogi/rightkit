// Consumer source contracts (CA-04, CA-07, CA-08). Every negative is SellRight's own bytes at the commit that failed (verbatim, or a
// verbatim excerpt where the file is long), every repair is the bytes or tree that went green, and every positive control is a
// legitimate shape the rule must keep accepting. The early-invocation tests run the shared entry points: `rightkit-gate check` and the
// right-release guard that refuses before any build command starts.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, guardCommand } from "./app-check.mjs";
import { CONSUMER_SOURCE_RULES, caddyBlocks, commandInputs, shellCommands, yamlEntries } from "./consumer-source-contracts.mjs";

const PACKAGE = path.dirname(fileURLToPath(import.meta.url));
const rule = (id) => CONSUMER_SOURCE_RULES.find((r) => r.id === id);

/** Writes `tree` ({ path: text }) to a temp root and runs rule `id` over it; `extra` lists tracked paths that need no bytes. */
function runRule(t, id, tree, extra = []) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-src-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [rel, text] of Object.entries(tree)) {
    fs.mkdirSync(path.join(root, path.dirname(rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  return rule(id).run({ root, files: [...Object.keys(tree), ...extra].sort() });
}

// ---- CA-04 -------------------------------------------------------------------------------------------------------------------
// Orthic-Labs/sellright .github/workflows/codeql.yml lines 22-36 at f5f45f7 (green), and dependabot's PR heads e5e403b (#263, init
// alone to v4.39.0; run 39882075486 / job 120181527212) and a03ba5f (#264, analyze alone; run 39777742058 / job 119820892454).
const codeql = (init, analyze) => `name: CodeQL

on:
  pull_request:
    branches: [main]

jobs:
  analyze:
    name: JavaScript / TypeScript security analysis
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1

      - uses: github/codeql-action/init@${init}
        with:
          languages: javascript-typescript
          queries: security-extended

      - uses: github/codeql-action/analyze@${analyze}
        with:
          category: '/language:javascript-typescript'
`;
const V4383 = "24c54180a607b1449ed407dd24f251e4e9147c8d # v4.38.3";
const V4390 = "6f2ac81f5cd81fe351a197ba0b00d91d03cd2aee # v4.39.0";

test("workflow-action-pair (CA-04): SellRight #263 and #264 (init or analyze bumped alone) are red; the pinned pair and a bumped pair are green", (t) => {
  const wf = ".github/workflows/codeql.yml";
  const pr263 = runRule(t, "workflow-action-pair", { [wf]: codeql(V4390, V4383) });
  assert.equal(pr263.length, 1);
  assert.equal(pr263[0].subject, "analyze:github/codeql-action");
  assert.equal(pr263[0].problem, ".github/workflows/codeql.yml:20 job analyze runs github/codeql-action/analyze@24c54180a607 (v4.38.3) but github/codeql-action/init@6f2ac81f5cd8 (v4.39.0) (line 15): its steps must come from one release, or the job fails at analyze");
  const pr264 = runRule(t, "workflow-action-pair", { [wf]: codeql(V4383, V4390) });
  assert.equal(pr264.length, 1);
  assert.match(pr264[0].problem, /analyze@6f2ac81f5cd8 \(v4\.39\.0\) but github\/codeql-action\/init@24c54180a607 \(v4\.38\.3\)/);
  // Repair (f5f45f7, and what merging both PRs together gives): one release for both steps.
  assert.deepEqual(runRule(t, "workflow-action-pair", { [wf]: codeql(V4383, V4383) }), []);
  assert.deepEqual(runRule(t, "workflow-action-pair", { [wf]: codeql(V4390, V4390) }), []);
  // A floating tag beside a pinned commit is two refs too (the tag moves); the job before f5f45f7 used @v4 for both: green.
  assert.equal(runRule(t, "workflow-action-pair", { [wf]: codeql("v4", V4383) }).length, 1);
  assert.deepEqual(runRule(t, "workflow-action-pair", { [wf]: codeql("v4", "v4") }), []);
});

test("workflow-action-pair (CA-04) positive controls: separate jobs, upload-sarif and other repositories keep their own refs", (t) => {
  const wf = `jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: github/codeql-action/init@${V4390}
      - uses: github/codeql-action/analyze@${V4390}
      - uses: github/codeql-action/upload-sarif@${V4383}
      - uses: actions/cache/restore@1bd1e32a3bdc45362d1e726936510720a7c30a57 # v4.2.0
      - uses: actions/cache/save@5a3ec84eff668545956fd18022155c47e93e2684 # v4.2.3
  other:
    runs-on: ubuntu-latest
    steps:
      - uses: github/codeql-action/init@${V4383}
      - uses: github/codeql-action/analyze@${V4383}
`;
  assert.deepEqual(runRule(t, "workflow-action-pair", { ".github/workflows/scan.yml": wf }), []);
});

// ---- CA-07 -------------------------------------------------------------------------------------------------------------------
// Verbatim: packages/api/package.json scripts at 311fdf6 (the build names scripts/write-build-info.mjs, committed 14 s later in
// 7b0a8e8), the root package.json at 31c4bd3 (deps:* run ../tools/right-release/deps.mjs, a sibling checkout no CI runner has), and
// .github/workflows/ci.yml lines 124-147 at eedf4f3 (packages/storefront moved out; the job still caches and installs it).
const API_SCRIPTS_311FDF6 = {
  dev: "tsx watch src/index.ts",
  build: "tsc -p tsconfig.json && node scripts/write-build-info.mjs",
  typecheck: "tsc -p tsconfig.json --noEmit",
  start: "node dist/index.js",
  test: "vitest run --project unit",
  "db:generate": "drizzle-kit generate",
  "postdb:generate": "node scripts/assert-no-partial-index-drop.mjs",
  "db:migrate:runtime": "node dist/scripts/migrate.js",
  "bootstrap:runtime": "node dist/scripts/bootstrap.js",
  "db:assert-rls": "tsx src/db/assert-force-rls.ts",
  "assert:shop-isolation": "node scripts/assert-shop-route-isolation.mjs",
  "pack:artifact": "pnpm build && pnpm pack",
  "gen:exports-contract": "node scripts/gen-exports-contract.mjs",
};
const API_TRACKED = ["packages/api/scripts/assert-no-partial-index-drop.mjs", "packages/api/scripts/assert-shop-route-isolation.mjs", "packages/api/scripts/gen-exports-contract.mjs", "packages/api/src/index.ts"];
const ROOT_31C4BD3 = `{
  "name": "sellright",
  "private": true,
  "packageManager": "pnpm@10.34.1",
  "engines": {
    "node": ">=20"
  },
  "scripts": {
    "build": "pnpm -r build && pnpm --dir packages/admin build",
    "typecheck": "pnpm -r typecheck && pnpm --dir packages/admin typecheck",
    "test": "pnpm --filter @sellright/api test",
    "verify": "pnpm -r build && pnpm -r typecheck && pnpm --dir packages/admin typecheck && pnpm --dir packages/admin build && pnpm --filter @sellright/api test && pnpm --filter @sellright/api db:assert-rls && pnpm --filter @sellright/api assert:shop-isolation",
    "dev": "pnpm --filter @sellright/api dev",
    "admin:typecheck": "pnpm --dir packages/admin typecheck",
    "admin:build": "pnpm --dir packages/admin build",
    "deps:check": "node ../tools/right-release/deps.mjs --check",
    "deps:audit": "node ../tools/right-release/deps.mjs --audit",
    "deps:update": "node ../tools/right-release/deps.mjs --update",
    "deps:update:latest": "node ../tools/right-release/deps.mjs --update --latest"
  },
  "pnpm": {
    "overrides": {
      "esbuild": "0.28.1"
    }
  },
  "devDependencies": {
    "typescript": "^6.0.3"
  }
}
`;
const CI_EEDF4F3 = `jobs:
  storefront:
    name: Storefront build / Node 24 LTS
    runs-on: ubuntu-latest
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1

      - uses: pnpm/setup@703c52620218391530e48b9e8870d5c0082e1b9b # v2.1.0
        with:
          version: \${{ env.PNPM_VERSION }}
          install: false

      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: \${{ env.NODE_LTS }}
          cache: pnpm
          cache-dependency-path: packages/storefront/pnpm-lock.yaml

      - name: Install storefront
        run: pnpm --dir packages/storefront install --frozen-lockfile

      - name: Build storefront
        run: pnpm --dir packages/storefront build
`;

test("referenced-input (CA-07): SellRight's missing write-build-info.mjs is red at 311fdf6 and green once committed (7b0a8e8)", (t) => {
  const pkg = JSON.stringify({ name: "@sellright/api", scripts: API_SCRIPTS_311FDF6 }, null, 2);
  const red = runRule(t, "referenced-input", { "packages/api/package.json": pkg }, API_TRACKED);
  assert.deepEqual(red.map((f) => f.subject), ["script:build:file:packages/api/scripts/write-build-info.mjs"]);
  assert.equal(red[0].problem, 'packages/api/package.json:5 script "build" runs node scripts/write-build-info.mjs: packages/api/scripts/write-build-info.mjs is not in this commit');
  // Positive controls in the same file: dist/ outputs (start, db:migrate:runtime) are produced by the build, not committed.
  assert.deepEqual(runRule(t, "referenced-input", { "packages/api/package.json": pkg }, [...API_TRACKED, "packages/api/scripts/write-build-info.mjs"]), []);
});

test("referenced-input (CA-07): a script that runs a sibling checkout (../tools/right-release/deps.mjs, 31c4bd3) is red; pnpm commands are green", (t) => {
  const tracked = ["packages/admin/package.json", "packages/api/package.json"];
  const red = runRule(t, "referenced-input", { "package.json": ROOT_31C4BD3 }, tracked);
  assert.deepEqual(red.map((f) => f.subject), ["script:deps:check:file:../tools/right-release/deps.mjs", "script:deps:audit:file:../tools/right-release/deps.mjs", "script:deps:update:file:../tools/right-release/deps.mjs", "script:deps:update:latest:file:../tools/right-release/deps.mjs"]);
  assert.match(red[0].problem, /^package\.json:16 script "deps:check" runs node \.\.\/tools\/right-release\/deps\.mjs --check: \.\.\/tools\/right-release\/deps\.mjs resolves to \.\.\/tools\/right-release\/deps\.mjs, outside the repository/);
  // The repair SellRight committed (023bce7): the same scripts through pnpm, against packages that exist.
  const repaired = ROOT_31C4BD3.replace(/node \.\.\/tools\/right-release\/deps\.mjs --check/, "pnpm outdated -r || true; pnpm --dir packages/admin outdated || true")
    .replace(/node \.\.\/tools\/right-release\/deps\.mjs --audit/, "pnpm audit --prod && pnpm --dir packages/admin audit --prod")
    .replace(/node \.\.\/tools\/right-release\/deps\.mjs --update --latest/, "pnpm up -r --latest && pnpm --dir packages/admin up --latest")
    .replace(/node \.\.\/tools\/right-release\/deps\.mjs --update/, "pnpm up -r && pnpm --dir packages/admin up");
  assert.deepEqual(runRule(t, "referenced-input", { "package.json": repaired }, tracked), []);
});

test("referenced-input (CA-07): eedf4f3's storefront job (cache path and pnpm --dir of a removed package) is red; with the package present it is green", (t) => {
  const wf = ".github/workflows/ci.yml";
  const red = runRule(t, "referenced-input", { [wf]: CI_EEDF4F3 }, ["pnpm-lock.yaml", "packages/api/package.json"]);
  assert.deepEqual(red.map((f) => f.subject), ["cache-dependency-path:packages/storefront/pnpm-lock.yaml", "run:dir:packages/storefront"]);
  assert.equal(red[0].problem, ".github/workflows/ci.yml:18 job storefront: cache-dependency-path packages/storefront/pnpm-lock.yaml is not in this commit (setup refuses: the dependency lock is not found)");
  assert.equal(red[1].problem, ".github/workflows/ci.yml:21 job storefront step runs pnpm --dir packages/storefront install --frozen-lockfile: packages/storefront is not in this commit");
  assert.deepEqual(runRule(t, "referenced-input", { [wf]: CI_EEDF4F3 }, ["pnpm-lock.yaml", "packages/storefront/package.json", "packages/storefront/pnpm-lock.yaml"]), []);
});

test("referenced-input (CA-07) positive controls: ignored outputs, checkout paths, variables, globs, cd, fixtures and eval stay accepted", (t) => {
  const tree = {
    "packages/storefront/.gitignore": "# Build\nbuild\nserver\ndist-*\n",
    "packages/storefront/package.json": JSON.stringify({ scripts: {
      serve: "node server/entry.express.js", // SellRight's Qwik server output: ignored by packages/storefront/.gitignore
      preview: "node dist-demo/x.mjs",
      tools: "cd tools && node run.mjs",
      eval: "node -e \"require('./nope.js')\"",
      inline: "sh -c 'node missing.mjs'",
      tests: "node --test scripts/*.test.mjs",
      env: "node $SCRIPT",
      imp: "node --import tsx src/main.ts",
    } }),
    "packages/storefront/tools/run.mjs": "",
    "packages/storefront/src/main.ts": "",
    "fixtures/broken/package.json": JSON.stringify({ scripts: { x: "node missing.mjs" } }),
    ".github/workflows/compat.yml": `jobs:
  compat:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: packages/storefront
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          path: old
      - run: node ../../old/scripts/migrate.mjs
        working-directory: old/packages/api
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          cache-dependency-path: |
            **/pnpm-lock.yaml
            \${{ matrix.lock }}
      - run: |
          node tools/run.mjs
          cd tools
          node run.mjs
          node "$RUNNER_TEMP/x.mjs"
`,
    "packages/storefront/pnpm-lock.yaml": "",
  };
  assert.deepEqual(runRule(t, "referenced-input", tree), []);
});

test("shellCommands and commandInputs read only certain paths", () => {
  assert.deepEqual(shellCommands("tsc -p a && node scripts/x.mjs --flag; pnpm --dir packages/a build | cat"), [["tsc", "-p", "a"], ["node", "scripts/x.mjs", "--flag"], ["pnpm", "--dir", "packages/a", "build"], ["cat"]]);
  assert.deepEqual(commandInputs(["node", "--import", "tsx", "src/a.ts"]), [{ kind: "file", value: "src/a.ts" }]);
  assert.deepEqual(commandInputs(["NODE_ENV=production", "node", "server"]), [], "a bare word with no slash or extension may be a package; not judged");
  assert.deepEqual(commandInputs(["npm", "--prefix=web", "ci"]), [{ kind: "dir", value: "web" }]);
  assert.deepEqual(commandInputs(["bash", "-c", "x.sh"]), []);
  const entries = yamlEntries("jobs:\n  a:\n    steps:\n      - uses: x/y@1\n        with:\n          path: p\n      - run: |\n          one\n          two\n");
  assert.deepEqual(entries.map((e) => [e.path.join("."), e.items[3] ?? null]), [["jobs", null], ["jobs.a", null], ["jobs.a.steps", null], ["jobs.a.steps.uses", entries[3].items[3]], ["jobs.a.steps.with", entries[3].items[3]], ["jobs.a.steps.with.path", entries[3].items[3]], ["jobs.a.steps.run", entries[6].items[3]]]);
  assert.notEqual(entries[3].items[3], entries[6].items[3], "two steps are two items");
  assert.deepEqual(entries[6].block.map((b) => b.text), ["one", "two"]);
});

// ---- CA-08 -------------------------------------------------------------------------------------------------------------------
// deploy/Caddyfile verbatim at 8b6e718 (run 36319625780 / job 108620824203 validated it with SELLRIGHT_DOMAIN unset: "an empty
// {$SELLRIGHT_DOMAIN} site address makes Caddy parse that block as GLOBAL OPTIONS ... then rejects 'encode'", e9b1a27) and at
// fe97ded (an explicit global options block first; current main).
const CADDY_8B6E718 = `{$SELLRIGHT_DOMAIN} {
    encode zstd gzip

    # The edge owns the untrusted socket and overwrites X-Real-IP before the
    # request enters the private admin/API network. Caddy also supplies the
    # normal X-Forwarded-Proto/Host chain used by nginx and the application.
    reverse_proxy admin:8080 {
        header_up X-Real-IP {remote_host}
    }
}

# TODO(WS-C): the storefront still bakes its API URL/theme in at build time
# (see packages/storefront/Dockerfile), so today it only serves usefully as a
# second, separately-built image pointed at THIS deployment's API. Once WS-C
# lands per-store runtime settings, one generic storefront image can share a
# host with the admin (e.g. admin at a subpath) instead of needing its own
# domain. Until then this block is opt-in: it only binds when
# SELLRIGHT_STOREFRONT_DOMAIN is set in deploy/.env, so installs that don't
# set it get byte-identical behavior to before the storefront service existed.
{$SELLRIGHT_STOREFRONT_DOMAIN:-:8090} {
    encode zstd gzip

    reverse_proxy storefront:4100 {
        header_up X-Real-IP {remote_host}
    }
}
`;
const CADDY_FE97DED = `{
    servers {
        trusted_proxies static 173.245.48.0/20 103.21.244.0/22
        client_ip_headers CF-Connecting-IP
        trusted_proxies_strict
    }
}

{$SELLRIGHT_DOMAIN} {
    encode zstd gzip

    reverse_proxy admin:8080 {
        header_up X-Real-IP {client_ip}
        header_up -CF-Connecting-IP
    }
}

{$SELLRIGHT_STOREFRONT_DOMAIN:-:8090} {
    encode zstd gzip

    reverse_proxy storefront:4100 {
        header_up X-Real-IP {client_ip}
        header_up -CF-Connecting-IP
    }
}
`;

test("caddy-global-options (CA-08): 8b6e718's env-only first address is red; fe97ded's explicit global block is green", (t) => {
  const red = runRule(t, "caddy-global-options", { "deploy/Caddyfile": CADDY_8B6E718 });
  assert.equal(red.length, 1);
  assert.equal(red[0].subject, "address:SELLRIGHT_DOMAIN");
  assert.equal(red[0].problem, "deploy/Caddyfile:1 the first block's address is only {$SELLRIGHT_DOMAIN}: with SELLRIGHT_DOMAIN unset or empty Caddy reads the block as global options and refuses `encode` (line 2) as an unrecognized global option");
  assert.deepEqual(runRule(t, "caddy-global-options", { "deploy/Caddyfile": CADDY_FE97DED }), []);
  // What Caddy saw in that run, written out: a global options block holding `encode`.
  const literal = runRule(t, "caddy-global-options", { "deploy/Caddyfile": CADDY_8B6E718.replace("{$SELLRIGHT_DOMAIN} {", "{") });
  assert.deepEqual(literal.map((f) => f.subject), ["global:encode"]);
});

test("caddy-global-options (CA-08) positive controls: a default, a literal address, plugin global options and nested braces stay accepted", (t) => {
  for (const text of [
    "{$SELLRIGHT_DOMAIN:localhost} {\n    encode gzip\n}\n",
    "example.com {\n    encode gzip\n}\n",
    "{\n    email ops@example.com\n    order rate_limit before basicauth\n    layer4 {\n        :443 {\n            route\n        }\n    }\n    log {\n        output stdout\n    }\n}\n{$DOMAIN} {\n    respond \"ok\"\n}\n",
    "{$A} {$B} {\n    file_server\n}\n".replace("{$A} {$B}", "{$A}, example.com"),
  ]) assert.deepEqual(runRule(t, "caddy-global-options", { Caddyfile: text }), [], text);
  assert.deepEqual(runRule(t, "caddy-global-options", { "test/fixtures/Caddyfile": "{\n    encode gzip\n}\n" }), [], "a fixture tree is not judged");
  assert.deepEqual(caddyBlocks("a {\n  reverse_proxy x {\n    header_up X {remote_host}\n  }\n}\n").map((b) => [b.header, b.directives.map((d) => d.name)]), [[["a"], ["reverse_proxy"]]]);
});

// ---- early invocation ----------------------------------------------------------------------------------------------------------
test("the consumer rules are app-check rules: rightkit-gate check reports them and right-release refuses a build before it starts", (t) => {
  for (const r of CONSUMER_SOURCE_RULES) assert.equal(RULES.find((x) => x.id === r.id), r, `${r.id} is in RULES`);
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-gate-")));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t"); git("config", "core.autocrlf", "false");
  fs.mkdirSync(path.join(repo, ".github/workflows"), { recursive: true });
  fs.writeFileSync(path.join(repo, ".github/workflows/codeql.yml"), codeql(V4390, V4383));
  git("add", "-A"); git("commit", "-q", "-m", "dependabot #263");
  const gate = spawnSync(process.execPath, [path.join(PACKAGE, "bin", "rightkit-gate.mjs"), "check", "--repo", repo, "--commit", "HEAD"], { encoding: "utf8" });
  assert.equal(gate.status, 1, gate.stderr);
  assert.match(gate.stdout, /^FAIL workflow-action-pair \(CA-04\): \.github\/workflows\/codeql\.yml:20 job analyze runs github\/codeql-action\/analyze@24c54180a607 \(v4\.38\.3\) but github\/codeql-action\/init@6f2ac81f5cd8 \(v4\.39\.0\)/m);
  const refused = guardCommand({ args: ["build"], cwd: repo });
  assert.equal(refused?.exitCode, 1);
  assert.ok(refused.lines.some((l) => l.startsWith("FAIL workflow-action-pair (CA-04)")), refused.lines.join("\n"));
  assert.match(refused.lines.at(-1), /^right-release build: the app check found problems in .*; nothing was started/);
  fs.writeFileSync(path.join(repo, ".github/workflows/codeql.yml"), codeql(V4390, V4390));
  git("commit", "-q", "-am", "bump both");
  const green = spawnSync(process.execPath, [path.join(PACKAGE, "bin", "rightkit-gate.mjs"), "check", "--repo", repo, "--commit", "HEAD", "--only", CONSUMER_SOURCE_RULES.map((r) => r.id).join(",")], { encoding: "utf8" });
  assert.equal(green.status, 0, green.stdout + green.stderr);
});
