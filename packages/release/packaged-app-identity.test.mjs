import assert from "node:assert/strict";
import test from "node:test";
import { assertPackagedIdentity, parseCodesignDisplay, parseLipoArchs } from "./packaged-app-identity.mjs";

const DEVID = `Executable=/x/App.app/Contents/MacOS/app
Identifier=app.example.main
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=30+7 location=embedded
Signature size=9000
Authority=Developer ID Application: Example (ABCDE12345)
Authority=Developer ID Certification Authority
CDHash=AbCd1234
TeamIdentifier=ABCDE12345
`;
const ADHOC = `Identifier=app.example.main
CodeDirectory v=20400 size=500 flags=0x2(adhoc) hashes=10+2 location=embedded
Signature=adhoc
TeamIdentifier=not set
`;

test("parses a Developer ID signature", () => {
  const d = parseCodesignDisplay(DEVID);
  assert.equal(d.identifier, "app.example.main");
  assert.equal(d.team, "ABCDE12345");
  assert.equal(d.cdhash, "abcd1234");
  assert.equal(d.runtime, true);
  assert.equal(d.adhoc, false);
  assert.equal(d.authorities.length, 2);
});

test("passes when declared expectations match", () => {
  const d = parseCodesignDisplay(DEVID);
  assert.deepEqual(assertPackagedIdentity(d, ["arm64", "x86_64"], { identifier: "app.example.main", team: "ABCDE12345", cdhash: "ABCD1234", archs: ["arm64"] }), []);
});

test("rejects ad-hoc, wrong team, wrong identifier, bad cdhash, missing arch", () => {
  const failures = assertPackagedIdentity(parseCodesignDisplay(ADHOC), ["arm64"], { identifier: "other", team: "ABCDE12345", cdhash: "ffff", archs: ["x86_64"] });
  const text = failures.join("\n");
  assert.match(text, /ad-hoc/);
  assert.match(text, /Identifier must be other/);
  assert.match(text, /TeamIdentifier must be ABCDE12345/);
  assert.match(text, /CDHash must be ffff/);
  assert.match(text, /missing architecture x86_64/);
});

test("allowAdhoc skips the ad-hoc and runtime checks only", () => {
  const failures = assertPackagedIdentity(parseCodesignDisplay(ADHOC), [], { allowAdhoc: true, identifier: "app.example.main" });
  assert.deepEqual(failures, []);
});

test("parseLipoArchs splits architectures", () => {
  assert.deepEqual(parseLipoArchs("x86_64 arm64\n"), ["x86_64", "arm64"]);
});
