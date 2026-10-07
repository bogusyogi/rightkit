import assert from "node:assert/strict";
import test from "node:test";
import { normalizeRepositoryIdentity, protectedOwnershipRoots } from "./ownership-registration.mjs";

test("repository identity normalization collapses HTTPS, SSH, and SCP GitHub remotes", () => {
  const expected = "https://github.com/bogusyogi/coderight";
  for (const remote of ["https://github.com/bogusyogi/CodeRight.git", "ssh://git@github.com/bogusyogi/coderight.git", "git@github.com:bogusyogi/coderight.git"]) {
    assert.equal(normalizeRepositoryIdentity(remote), expected);
  }
  assert.throws(() => normalizeRepositoryIdentity("https://user:secret@github.com/a/b.git"), /credential-bearing/);
});

test("protected registry is authoritative for exact nested roots and visibility", () => {
  const trust = { repositories: [{ repositoryIdentity: "https://github.com/bogusyogi/coderight", roots: ["engine", "apps/ui"], visibility: "private" }] };
  assert.deepEqual(protectedOwnershipRoots(trust, { repositoryIdentity: "git@github.com:bogusyogi/coderight.git", scanRoot: "C:/repo/engine", repoRoot: "C:/repo" }), {
    repositoryIdentity: "https://github.com/bogusyogi/coderight", visibility: "private", roots: ["engine", "apps/ui"],
  });
  assert.throws(() => protectedOwnershipRoots(trust, { repositoryIdentity: "https://github.com/bogusyogi/coderight", scanRoot: "C:/repo/src", repoRoot: "C:/repo" }), /not registered/);
  assert.throws(() => protectedOwnershipRoots({ repositories: [] }, { repositoryIdentity: "https://github.com/a/b", scanRoot: ".", repoRoot: "." }), /not registered/);
});
