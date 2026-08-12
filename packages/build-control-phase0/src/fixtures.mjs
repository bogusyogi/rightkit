import { mkdirSync, writeFileSync, chmodSync } from 'node:fs';
import { join } from 'node:path';

function file(root, name, content, executable = false) {
  const path = join(root, name); mkdirSync(join(path, '..'), { recursive: true }); writeFileSync(path, content);
  if (executable) chmodSync(path, 0o755);
}
export function createFixture(kind, root) {
  mkdirSync(root, { recursive: true });
  if (kind === 'cargo') {
    file(root, 'Cargo.toml', '[package]\nname="phase0-cargo"\nversion="0.1.0"\nedition="2024"\nbuild="build.rs"\n');
    file(root, 'build.rs', 'fn main(){std::fs::write(std::env::var("OUT_DIR").unwrap()+"/phase0.txt","generated").unwrap()}');
    file(root, 'src/main.rs', 'fn main(){assert_eq!(include_str!(concat!(env!("OUT_DIR"),"/phase0.txt")),"generated")} #[cfg(test)] mod t { #[test] fn works(){assert!(true)} }');
  } else if (kind === 'swiftpm') {
    file(root, 'Package.swift', '// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name:"Phase0", targets:[.executableTarget(name:"Phase0"), .testTarget(name:"Phase0Tests",dependencies:["Phase0"])])\n');
    file(root, 'Sources/Phase0/main.swift', 'print("generated executable")\n'); file(root, 'Tests/Phase0Tests/Phase0Tests.swift', 'import Testing\n@Test func works(){ #expect(true) }\n');
  } else if (kind === 'node-pnpm') {
    file(root, 'package.json', '{"name":"phase0-node","version":"0.0.0","scripts":{"build":"node scripts/build.mjs","test":"node --test test.mjs","generated":"node generated.mjs"}}');
    file(root, 'scripts/build.mjs', 'import {writeFileSync} from "node:fs";writeFileSync("generated.mjs","console.log(\\"generated executable\\")\\n")'); file(root, 'test.mjs', 'import test from "node:test";import assert from "node:assert/strict";test("works",()=>assert.equal(1,1))');
  } else if (kind === 'node-gyp') {
    file(root, 'package.json', '{"name":"phase0-node-gyp","version":"0.0.0","private":true,"devDependencies":{"node-gyp":"12.1.0"},"scripts":{"rebuild":"node-gyp rebuild","test":"node -e \\\"require(\'./build/Release/phase0.node\')\\\"","generated":"node -e \\\"require(\'./build/Release/phase0.node\')\\\""}}');
    file(root, 'binding.gyp', '{"targets":[{"target_name":"phase0","sources":["phase0.cc"]}]}'); file(root, 'phase0.cc', '#include <node.h>\nvoid Init(v8::Local<v8::Object> e){ } NODE_MODULE(NODE_GYP_MODULE_NAME, Init)\n');
  } else if (kind === 'cmake') {
    file(root, 'CMakeLists.txt', 'cmake_minimum_required(VERSION 3.15)\nproject(phase0 C)\nadd_executable(phase0 main.c)\nenable_testing()\nadd_test(NAME phase0 COMMAND phase0)\n'); file(root, 'main.c', '#include <stdio.h>\nint main(){puts("generated executable");return 0;}\n');
  } else if (kind === 'make') {
    file(root, 'Makefile', 'all: phase0\nphase0: main.c\n\t$(CC) -o $@ $<\ntest: phase0\n\t./phase0\ngenerated: phase0\n\t./phase0\nclean:\n\trm -f phase0\n'); file(root, 'main.c', '#include <stdio.h>\nint main(){puts("generated executable");return 0;}\n');
  } else if (kind === 'raw-compiler') file(root, 'main.c', '#include <stdio.h>\nint main(){puts("generated executable");return 0;}\n');
  return root;
}
