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

test("any secret but GITHUB_TOKEN is refused, however it is spelled", () => {
  for (const reference of ["secrets.APPLE_API_KEY", "secrets . ASC_KEY_ID", "secrets['P12']", "toJSON(secrets)"]) {
    assert.match(refusal({ "ci.yml": `${WEB_CI}      - run: echo \${{ ${reference} }}\n` }), /secrets|whole secrets context/, reference);
  }
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
