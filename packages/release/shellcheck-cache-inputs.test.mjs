import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildCacheDigest } from "./bin/rightkit-gate.mjs";

const scan = {
  id: "shellcheck", class: "A", tool: "shellcheck", root: "content",
  args: ["--format=gcc", "--external-sources", "scripts/check.sh"],
};

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-shellcheck-key-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const files = new Set();
  const write = (name, content) => {
    const destination = path.join(root, name);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, content);
    files.add(name);
  };
  const readBlob = (name) => fs.readFileSync(path.join(root, name), "utf8");
  const objectId = (name) => files.has(name) ? createHash("sha256").update(readBlob(name)).digest("hex") : null;
  const digest = (shellcheckEnv = {}) => {
    let reason;
    const key = buildCacheDigest({
      objectId, listFiles: () => [...files], readBlob, shellcheckRoot: root, shellcheckEnv,
      onKeyError: (_row, error) => assert.fail(error),
      onUncacheable: (row, why) => { assert.equal(row.id, scan.id); reason = why; },
    })(scan);
    return { key, reason };
  };
  write(".shellcheckrc", "disable=SC1091\n");
  return { write, digest };
}

test("ShellCheck key binds entry, tracked configuration and transitive literal sources", (t) => {
  const { write, digest } = fixture(t);
  write("scripts/check.sh", "#!/bin/sh\nset -eu\ncd .\nprintf '%s\\n' 'source outside'\n. scripts/lib.inc\n");
  write("scripts/lib.inc", "source scripts/leaf.inc\n");
  write("scripts/leaf.inc", "VALUE=one\n");
  const initial = digest();
  assert.equal(initial.reason, undefined);
  assert.deepEqual(JSON.parse(initial.key).files.map(([name]) => name), [
    ".shellcheckrc", "scripts/check.sh", "scripts/leaf.inc", "scripts/lib.inc",
  ]);
  write("scripts/leaf.inc", "VALUE=two\n");
  const afterLeaf = digest().key;
  assert.notEqual(afterLeaf, initial.key);
  write(".shellcheckrc", "disable=SC1090\n");
  assert.notEqual(digest().key, afterLeaf);
});

test("ShellCheck reports uncacheable rows with sources or options it cannot bind", (t) => {
  const { write, digest } = fixture(t);
  for (const shell of ['. "$SOURCE"', "source ../outside.inc", "if true; then cd .; fi"]) {
    write("scripts/check.sh", `#!/bin/sh\n${shell}\n`);
    const result = digest();
    assert.equal(result.key, null);
    assert.match(result.reason, /scripts\/check\.sh:2: ShellCheck read set is unproven/);
  }
  write("scripts/check.sh", "#!/bin/sh\ncd .\n");
  write("scripts/.shellcheckrc", "source-path=../outside\n");
  assert.match(digest().reason, /scripts\/\.shellcheckrc:1: unsupported ShellCheck configuration/);
  assert.match(digest({ SHELLCHECK_OPTS: "--source-path=../outside" }).reason, /SHELLCHECK_OPTS/);
});
