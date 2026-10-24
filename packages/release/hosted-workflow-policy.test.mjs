import test from "node:test";
import assert from "node:assert/strict";
import { declaredUnrelatedRefusal, declaredUnrelatedShapeProblems, releaseArtifactPaths } from "./hosted-workflow-policy.mjs";

const WEB_CI = `name: ci
on: { push: {}, pull_request: {} }
permissions:
  contents: read
  packages: write
jobs:
  model-contracts:
    runs-on: macos-15
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      - run: cp ios/Shared/Money.swift "$RUNNER_TEMP/pkg/Money.swift" && swift test --package-path "$RUNNER_TEMP/pkg"
  image:
    runs-on: ubuntu-latest
    steps:
      - uses: docker/login-action@0000000000000000000000000000000000000000
        with: { registry: ghcr.io, password: \${{ secrets.GITHUB_TOKEN }} }
`;

const declaration = (files) => ({ policy: "declared-unrelated", files });
const refusal = (sources, files = Object.fromEntries(Object.keys(sources).map((file) => [file, "web and server pipeline, unrelated to the iOS app"])), artifacts = ["build/ios/export/App.ipa"]) =>
  declaredUnrelatedRefusal({ declaration: declaration(files), workflows: Object.keys(sources), read: (file) => sources[file], artifacts });

test("a declared web pipeline that tests app sources with only GITHUB_TOKEN is admitted", () => {
  assert.equal(refusal({ "ci.yml": WEB_CI, "codeql.yml": "jobs: {}\n" }), null);
});

test("every workflow must be declared with a reason, and every declaration must exist", () => {
  assert.match(refusal({ "ci.yml": WEB_CI, "extra.yml": "jobs: {}\n" }, { "ci.yml": "web and server pipeline" }), /undeclared hosted workflow file\(s\): extra\.yml/);
  assert.match(refusal({ "ci.yml": WEB_CI }, { "ci.yml": "web and server pipeline", "gone.yml": "an old pipeline that was removed" }), /do not exist: gone\.yml/);
  assert.match(refusal({ "ci.yml": WEB_CI }, { "ci.yml": "web" }), /must say why the workflow exists/);
});

test("any secret but GITHUB_TOKEN is refused, and so is passing the secrets context on", () => {
  for (const reference of ["secrets.APPLE_API_KEY", "secrets . ASC_KEY_ID", "secrets['P12']", "toJSON(secrets)", "join(secrets)", "SECRETS.X"]) {
    assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: echo \${{ ${reference} }}\n` }), /references secrets other than GITHUB_TOKEN/, reference);
  }
  const inherit = `${WEB_CI}  ship:\n    uses: ./.github/workflows/ship.yml\n    secrets: inherit\n`;
  assert.match(refusal({ "ci.yml": inherit }), /secrets other than GITHUB_TOKEN/);
  // a path segment named secrets is not the context
  assert.equal(refusal({ "ci.yml": `${WEB_CI}      # reads /run/secrets/gateway_accounts\n` }), null);
});

test("a reusable workflow is refused even without secrets, since the called workflow is not checked", () => {
  assert.match(refusal({ "ci.yml": `${WEB_CI}  ship:\n    uses: someorg/tools/.github/workflows/ship.yml@0123456789abcdef0123456789abcdef01234567\n` }), /calls the reusable workflow/);
});

test("a YAML escape that could spell a secret or a command is refused", () => {
  assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: "echo \${{ s\\u0065crets.ASC_KEY }}"\n` }), /\\x, \\u or \\U escape/);
  assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: "xcrun notaryt\\x6fol submit a"\n` }), /escape/);
});

test("every action is commit-pinned (or local, or a docker image by digest)", () => {
  assert.match(refusal({ "ci.yml": `${WEB_CI}      - uses: actions/checkout@v4\n` }), /actions\/checkout@v4, which is not pinned/);
  assert.equal(refusal({ "ci.yml": `${WEB_CI}      - uses: ./.github/actions/setup\n      - uses: docker://alpine@sha256:${"a".repeat(64)}\n` }), null);
});

test("an OIDC token is refused unless the file declares why it mints one", () => {
  const oidc = `${WEB_CI.replace("  packages: write\n", "  packages: write\n  id-token: write\n")}`;
  assert.match(refusal({ "ci.yml": oidc }), /id-token: write\) without declaring why/);
  assert.equal(refusal({ "ci.yml": oidc }, { "ci.yml": { reason: "web and server pipeline", idToken: "cosign keyless signing of the images" } }), null);
  assert.match(declaredUnrelatedShapeProblems(declaration({ "ci.yml": { reason: "web and server pipeline", idToken: "x", extra: 1 } })).join("; "), /idToken must say why.*extra is not a known field|extra is not a known field.*idToken must say why/);
});

test("release machinery is refused even inside a comment or a quoted string", () => {
  const cases = {
    "xcodebuild -scheme App archive": /xcodebuild archive/,
    "xcodebuild -exportArchive -archivePath a": /xcodebuild archive/,
    "xcrun notarytool submit app.zip": /notarization/,
    "xcrun altool --upload-app": /App Store upload/,
    "codesign --sign - App.app": /codesign/,
    "pnpm exec right-release build --platform ios": /right-release/,
    "bundle exec fastlane beta": /fastlane/,
    "security import cert.p12": /keychain import/,
    "signtool sign app.exe": /Windows code signing/,
    "pnpm tauri build": /Tauri app build/,
    'echo " # "; codesign -s x a': /codesign/,
  };
  for (const [command, expected] of Object.entries(cases)) {
    assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: ${command}\n` }), expected, command);
  }
  assert.match(refusal({ "ci.yml": `${WEB_CI}# later: codesign the app here\n` }), /codesign/);
});

test("a workflow that names a release artifact is refused", () => {
  assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: ls build/ios/export/App.ipa\n` }), /names the release artifact build\/ios\/export\/App\.ipa/);
});

test("the declaration shape is closed", () => {
  assert.deepEqual(declaredUnrelatedShapeProblems(declaration({ "ci.yml": "web and server pipeline" })), []);
  assert.match(declaredUnrelatedShapeProblems({ policy: "declared-unrelated", files: {}, extra: 1 }).join("; "), /hostedWorkflows\.extra is not a known field.*files must map/);
  assert.match(declaredUnrelatedShapeProblems({ policy: "other", files: { "ci.yml": "web and server pipeline" } }).join(";"), /policy must be "declared-unrelated"/);
  assert.match(declaredUnrelatedShapeProblems(declaration({ "../ci.yml": "web and server pipeline" })).join(";"), /invalid workflow file name/);
});

test("release artifact paths come from every target's artifacts and iOS archive and ipa", () => {
  assert.deepEqual(releaseArtifactPaths({ targets: { ios: { artifacts: ["build/a.ipa"], iosPackage: { archive: "build/a.xcarchive", ipa: "build/a.ipa" } }, win: { artifacts: ["dist/a.exe"] } } }).sort(), ["build/a.ipa", "build/a.xcarchive", "dist/a.exe"]);
});
