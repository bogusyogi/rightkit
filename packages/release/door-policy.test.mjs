import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { claudeDoorDecision, doorDenial, doorViolation, simpleCommands } from "./door-policy.mjs";

test("one door: every way an agent could build an app outside rightkit-gate is named", () => {
  const denied = {
    "cargo build --release": "cargo build", "cargo test -p a": "cargo test", "cargo +1.98.0 clippy --all-targets": "cargo clippy", "cargo check": "cargo check",
    "RUSTFLAGS=-D cargo run": "cargo run", "env CI=1 cargo bench": "cargo bench", "cd app && cargo b": "cargo b", "timeout 600 cargo t": "cargo t",
    "pnpm tauri build": "tauri build", "pnpm exec tauri dev": "tauri dev", "npx tauri build --debug": "tauri build", "yarn tauri bundle": "tauri bundle",
    "pnpm --filter app exec tauri build": "tauri build", "cargo tauri build": "cargo tauri build",
    // Adrian, 2026-10-26: Apple, Android, frontend bundlers and build-named scripts are doors too.
    "xcodebuild -scheme App -configuration Release archive": "xcodebuild archive", "xcodebuild -project A.xcodeproj -scheme A": "xcodebuild build",
    "xcrun xcodebuild test -scheme A": "xcodebuild test", "swift build -c release": "swift build", "swift test": "swift test", "swiftc main.swift": "swiftc",
    "./gradlew assembleRelease": "gradle assembleRelease", "gradle bundleRelease": "gradle bundleRelease", "cd android && ./gradlew.bat build": "gradle build",
    "pnpm build": "pnpm build", "pnpm run build:mac": "pnpm build:mac", "npm run tauri": "npm tauri", "pnpm exec vite build": "vite build", "npx webpack --mode production": "webpack",
    "cargo tauri android build": "cargo tauri android", "pnpm tauri ios build": "tauri ios",
    // Astra, 2026-10-26: combined shell flags, option-prefixed tools, several Gradle tasks, vite options, and test scripts.
    "bash -lc 'cargo build'": "cargo build", "zsh -xc 'swift build'": "swift build", "xcrun --sdk iphoneos xcodebuild -scheme A archive": "xcodebuild archive",
    "swift --package-path Sources build": "swift build", "./gradlew tasks assembleRelease": "gradle assembleRelease", "vite --config vite.qa.ts --mode qa build": "vite build",
    "pnpm test": "pnpm test", "pnpm run test:e2e": "pnpm test:e2e", "npm run qa": "npm qa", "pnpm exec vite --config x build": "vite build",
    "right-release build --platform win": "right-release build", "npx right-release upload --release r --platform win --tier patch": "right-release upload",
    "node node_modules/@rightkit/release/cli/right-release.mjs qa --stage installed": "right-release qa", "echo hi; right-release package": "right-release package",
  };
  for (const [command, violation] of Object.entries(denied)) assert.equal(doorViolation(command), violation, command);
});

test("one door: the door itself, the broker, read-only cargo and ordinary commands are allowed", () => {
  for (const command of [
    "rightkit-gate --to compile", "pnpm exec rightkit-gate --to package --platform mac", "npx rightkit-gate check --commit HEAD",
    "rustright cargo build", "rustright cargo test -p a", "cargo fmt --all -- --check", "cargo metadata --no-deps --format-version 1",
    "cargo update -p serde --precise 1.0.1", "cargo tree", "cargo --version", "right-release check",
    "git commit -m 'cargo build fixed'", "echo \"pnpm tauri build\"", "grep -rn 'cargo test' docs", "pnpm install --frozen-lockfile", "pnpm run lint", "",
    "xcodebuild -version", "xcodebuild -list -project A.xcodeproj", "xcodebuild -showsdks", "swift package resolve", "swift --version", "./gradlew --version", "gradle tasks", "gradlew dependencies",
    "pnpm run lint:fix", "pnpm exec tsc --noEmit", "vite --version", "xcrun --sdk iphoneos --show-sdk-path", "swift --package-path Sources package resolve", "./gradlew tasks --all",
  ]) assert.equal(doorViolation(command), null, command);
});

test("one door: the denial names the stage to run, and the Claude hook denies through PreToolUse", () => {
  assert.match(doorDenial("cargo test"), /Required: rightkit-gate --to compile /);
  assert.match(doorDenial("tauri build"), /rightkit-gate --to package --platform mac\|win/);
  assert.match(doorDenial("right-release qa"), /--to qualify --platform mac\|win/);
  assert.match(doorDenial("right-release upload"), /--to release --platform mac\|win --tier patch\|update/);
  assert.match(doorDenial("xcodebuild archive"), /--to package --platform ios/);
  assert.match(doorDenial("gradle assembleRelease"), /--to package --platform android/);
  assert.match(doorDenial("swift build"), /--to compile /);
  assert.match(doorDenial("pnpm build"), /--to compile /);
  assert.match(doorDenial("cargo tauri android"), /--to package --platform android/);
  const deny = claudeDoorDecision({ tool_name: "Bash", tool_input: { command: "cargo build" } });
  assert.equal(deny.hookSpecificOutput.permissionDecision, "deny");
  assert.match(deny.hookSpecificOutput.permissionDecisionReason, /^\[HOOK:one-door:block\]/);
  assert.deepEqual(claudeDoorDecision({ tool_name: "Bash", tool_input: { command: "rightkit-gate --to compile" } }), {});
  assert.deepEqual(claudeDoorDecision({ tool_name: "Read" }), {});
  assert.deepEqual(simpleCommands("a 'b c' && d \"e;f\" | g"), [["a", "b c"], ["d", "e;f"], ["g"]]);
});

test("the agent hook binary: stdin JSON in, the decision out; unreadable input allows", () => {
  const bin = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin", "rightkit-agent-hook.mjs");
  const run = (input) => JSON.parse(spawnSync(process.execPath, [bin], { input, encoding: "utf8" }).stdout);
  assert.equal(run(JSON.stringify({ tool_name: "Bash", tool_input: { command: "pnpm tauri build" } })).hookSpecificOutput.permissionDecision, "deny");
  assert.deepEqual(run(JSON.stringify({ tool_name: "Bash", tool_input: { command: "ls" } })), {});
  assert.deepEqual(run("not json"), {});
});

test("dev and test compile too: right-release dev|test and tauri dev are denied with the --to dev|test door", () => {
  for (const [command, door] of [["pnpm exec right-release dev --platform mac", "--to dev"], ["right-release test", "--to test"], ["npx tauri dev", "--to dev"], ["right-release stage-sidecar", "--to package"]]) {
    const v = doorViolation(command);
    assert.ok(v, command);
    assert.ok(doorDenial(v).includes(`rightkit-gate ${door}`), `${command}: ${doorDenial(v)}`);
  }
  assert.equal(doorViolation("rightkit-gate --to dev -- --platform mac"), null);
});

test("wrapped and option-prefixed forms are judged by what they run", () => {
  for (const command of ["bash -c 'cargo build --release'", "sh -c \"cd app && cargo test\"", "eval cargo clippy", "cargo -C crates/x build", "cargo --config net.offline=true test", "cargo --color always check", "echo x | xargs cargo build", "right-release --platform mac", "node node_modules/@rightkit/release/cli/right-release.mjs --platform win", "right-release cargo-target", "right-release dmg build", "node node_modules/@rightkit/release/build-release.mjs"]) {
    assert.ok(doorViolation(command), command);
  }
  for (const command of ["bash -c 'git status'", "cargo -C crates/x metadata", "right-release --help", "sh -c 'rightkit-gate --to compile'"]) assert.equal(doorViolation(command), null, command);
});

test("shell forms the policy simulation (scripts/sim/door-policy.mjs) found allowed are judged by what bash runs", () => {
  const S = "'";
  for (const command of [
    "{ cargo build; }", "if true; then cargo test; fi", "for x in 1; do cargo build; done", "while true; do cargo check; break; done", "f() { cargo build; }; f",
    "echo \"$(cargo build)\"", "echo `cargo build`", "echo \"$(case x in x) pnpm exec tauri build;; esac)\"", "T=cargo; $T build", "T=cargo; ${T} build",
    "echo build | xargs cargo", "nohup echo tauri dev | xargs npx >/dev/null", "sudo timeout 60 cargo clippy", "eval 'echo \"$(cargo build --release)\"'",
    `bash -c ${S}T=cargo; \${T} build${S}`, "true | T=rustright; ${T} cargo build", "false && T=rustright; $T cargo build", "(T=rustright); $T cargo build", "$UNKNOWN build",
  ]) assert.ok(doorViolation(command), command);
  for (const command of [
    "T=rustright; $T cargo build", "if true; then T=rustright; $T cargo build; fi", "false || T=rustright; $T cargo build", "time T=cargo; $T --version",
    "echo fmt --all | xargs cargo", "echo `T=pnpm; $T run lint`", "for x in 1; do T=pnpm; $T run lint; done", "echo \"cargo build\" > /dev/null",
  ]) assert.equal(doorViolation(command), null, command);
});
