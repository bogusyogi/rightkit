import assert from "node:assert/strict";
import test from "node:test";
import { registrySpecProblem, shippedDependencyProblems, SHIPPED_DEPENDENCY_FIELDS } from "./registry-spec.mjs";
import { planPublication } from "./publish.mjs";
import { checkNpmTarball, REPOSITORY } from "./upload.mjs";

const valid = ["*", "1", "1.2.x", "^1.2.3", "~0.3", ">= 1.2.3 < 3", "1.0.0 - 2.0.0", "^1 || >=2 <4", "|| >=1", "1.2.3-beta.4+build.9", ">=1.0.0-rc.1", "latest", "next-canary", "_canary", "npm:JSONStream@^1", "npm:other@^2", "npm:@scope/other@>=1.0.0-rc.1 <2", "npm:@scope/other@next", "npm:other"];
const invalid = ["file:../local", "link:../local", "workspace:*", "git://host/repo", "git+https://host/repo.git", "git+ssh://git@host/repo", "github:owner/repo", "gitlab:owner/repo", "bitbucket:owner/repo", "https://host/archive.tgz", "http://host/archive.tgz", "ssh://host/repo", "git@github.com:owner/repo.git", "owner/repo", "owner/repo#v1", "./local", "../local", "/local", "C:\\local", "npm:other@github:owner/repo", "npm:@scope/other@git+https://host/repo", "npm:other@npm:nested@1", "npm:../local@1", "npm:@scope/other@workspace:*", "npm:other@file:../local", "", null, {}, "1.2.3\n"];

invalid.push(".", "..", "owner\\repo", "https%3A%2F%2Fhost%2Frepo", "npm:", "npm:other@", "npm:@scope/@^1", "npm:.local@^1", "npm:owner/repo@^1", "npm:other@https://host/archive.tgz", "npm:other@owner/repo", "npm:other@1#fragment", "npm:other@npm:JSONStream@^1");
// Shared invalid matrix runs against classification, every shipped field,
// planner's external edge & parsed packed-manifest upload checks below.
for (const local of [".local-dir", "..local-dir", ".hidden", "payload.tgz", "payload.tar.gz", "payload.tar", "PAYLOAD.TGZ", "PAYLOAD.TAR.GZ", "PAYLOAD.TAR"]) {
  invalid.push(local, ` ${local} `, `npm:other@${local}`, `npm:@scope/other@ ${local} `);
}
for (const archive of ["payload.tgz", "payload.tar.gz", "payload.tar"]) invalid.push(`npm:${archive}`, `npm:${archive}@^1`);
valid.push("npm:@scope/payload.tgz@^1", "npm:@scope/payload.tar.gz@latest");

test("registry source classification preserves ranges, prereleases, tags & scoped aliases", () => {
  for (const spec of valid) assert.equal(registrySpecProblem(spec), null, spec);
  for (const spec of invalid) assert.ok(registrySpecProblem(spec), JSON.stringify(spec));
});

test("workspace exception requires actual member & never admits nested alternate sources", () => {
  for (const requirement of ["*", "^", "~", "^1.2.3", ">=1 <3"]) {
    assert.equal(registrySpecProblem(`workspace:${requirement}`, { workspaceMember: true }), null);
    assert.ok(registrySpecProblem(`workspace:${requirement}`));
  }
  for (const spec of ["workspace:file:../local", "workspace:github:owner/repo", "workspace:"]) assert.ok(registrySpecProblem(spec, { workspaceMember: true }));
});

test("packed shipped fields refuse every alternate source; development edges are excluded", () => {
  for (const field of SHIPPED_DEPENDENCY_FIELDS) {
    for (const spec of invalid) assert.equal(shippedDependencyProblems({ name: "pkg", [field]: { external: spec } }).length, 1, `${field} ${JSON.stringify(spec)}`);
    for (const spec of valid) assert.deepEqual(shippedDependencyProblems({ name: "pkg", [field]: { external: spec } }), []);
    for (const malformed of [null, [], "bad"]) assert.equal(shippedDependencyProblems({ name: "pkg", [field]: malformed }).length, 1);
  }
  assert.deepEqual(shippedDependencyProblems({ name: "pkg", devDependencies: { helper: "file:../dev-helper" } }), []);
});

test("planner checks external dependencies before workspace-target shortcut", async () => {
  const unit = { name: "@rightkit/fixture", version: "1.0.0", dir: "packages/fixture", hasTest: true, private: false };
  const fetchImpl = async () => ({ status: 404, ok: false });
  const plan = (spec, extra = []) => planPublication({ registry: "npm", unitsText: unit.name, dryRun: true, root: "/nonexistent-registry-spec-fixture", workspace: new Map([[unit.name, { ...unit, deps: [{ name: "external", spec }] }], ...extra]), fetchImpl });
  for (const spec of invalid) await assert.rejects(plan(spec), /dependency external/, JSON.stringify(spec));
  for (const spec of valid) assert.equal((await plan(spec)).units[0].name, unit.name);
  // Existing workspace rewrite still orders a real member before its consumer.
  const external = { name: "external", version: "1.0.0", dir: "packages/external", hasTest: true, private: false, deps: [] };
  const workspace = new Map([[unit.name, { ...unit, deps: [{ name: external.name, spec: "workspace:*" }] }], [external.name, external]]);
  const result = await planPublication({ registry: "npm", unitsText: `${unit.name} external`, dryRun: true, root: "/nonexistent-registry-spec-fixture", workspace, fetchImpl });
  assert.deepEqual(result.units.map(item => item.name), ["external", unit.name]);
});

test("tarball upload applies shared source guard to parsed packed manifest", () => {
  for (const field of SHIPPED_DEPENDENCY_FIELDS) {
    for (const spec of invalid) {
      const pkg = { name: "@rightkit/fixture", version: "1.0.0", repository: { url: `https://github.com/${REPOSITORY}`, directory: "packages/fixture" }, [field]: { external: spec } };
      const spawn = (_command, args) => ({ status: 0, stdout: args[0] === "-tzf" ? "package/package.json\n" : Buffer.from(JSON.stringify(pkg)) });
      const result = checkNpmTarball({ file: "fixture.tgz", name: pkg.name, version: pkg.version, dir: "packages/fixture", spawn });
      assert.equal(result.problems.length, 1, `${field} ${JSON.stringify(spec)}`);
      assert.match(result.problems[0], /external/);
    }
  }
});
