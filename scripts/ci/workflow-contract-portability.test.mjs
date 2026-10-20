import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { assertUploadScript, proveUploadScriptMutations } from "./workflow-contract.mjs";

const uploader = fs.readFileSync(new URL("./upload.mjs", import.meta.url), "utf8");

test("uploader approval contract and mutation proof are portable across LF and CRLF", () => {
  for (const source of [uploader.replace(/\r\n?/g, "\n"), uploader.replace(/\r\n?/g, "\n").replace(/\n/g, "\r\n")]) {
    assert.equal(assertUploadScript(source), true);
    assert.deepEqual(proveUploadScriptMutations(source), []);
  }
});
