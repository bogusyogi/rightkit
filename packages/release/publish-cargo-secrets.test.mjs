import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { PEM_PRIVATE_KEY, scanCrateForSecrets } from "./publish-cargo.mjs";

// The markers are assembled here so this file does not itself hold a header the repository's own scans would read.
const header = (kind = "") => ["-----BEGIN ", kind, "PRIVATE KEY-----"].join("");
const footer = (kind = "") => ["-----END ", kind, "PRIVATE KEY-----"].join("");
const body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj";

test("a PEM private key is its header followed by key data; the header alone is a format marker", () => {
  // A real key, on its own lines, in each header spelling.
  for (const kind of ["", "RSA ", "EC ", "OPENSSH ", "ENCRYPTED "]) assert.ok(PEM_PRIVATE_KEY.test(`${header(kind)}\n${body}\n${footer(kind)}\n`), kind || "PKCS#8");
  assert.ok(PEM_PRIVATE_KEY.test(`${header()}\r\n${body}\r\n`), "CRLF");
  // A key pasted into a string literal or JSON, line breaks escaped.
  assert.ok(PEM_PRIVATE_KEY.test(`const k = "${header()}\\n${body}\\n${footer()}";`), "escaped \\n");
  // Code that checks an uploaded key's shape names only the markers (rightkit-remote-gateway push_key.rs valid_p8).
  assert.equal(PEM_PRIVATE_KEY.test(`&& text.starts_with("${header()}")\n&& text.ends_with("${footer()}")`), false);
  assert.equal(PEM_PRIVATE_KEY.test(`"the key is the text of the .p8 file, from BEGIN PRIVATE KEY to END PRIVATE KEY"`), false);
  // A header followed by prose, or by too little to be key data, is not a key either.
  assert.equal(PEM_PRIVATE_KEY.test(`${header()}\nthen paste the rest of the file here\n`), false);
  assert.equal(PEM_PRIVATE_KEY.test(`${header()}\nMIIE\n`), false);
});

test("the crate scan flags a file holding a key and passes one that only names the markers", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pem-scan-"));
  try {
    fs.mkdirSync(path.join(dir, "src"));
    fs.writeFileSync(path.join(dir, "src", "push_key.rs"), `fn valid_p8(text: &str) -> bool {\n    text.starts_with("${header()}") && text.ends_with("${footer()}")\n}\n`);
    assert.deepEqual(scanCrateForSecrets(dir), []);
    fs.writeFileSync(path.join(dir, "src", "leak.rs"), `const KEY: &str = "${header()}\\n${body}\\n${footer()}";\n`);
    assert.deepEqual(scanCrateForSecrets(dir), ["src/leak.rs"]);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
