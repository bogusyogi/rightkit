import assert from "node:assert/strict";
import test from "node:test";
import { compareGradleVersions, gradleLockProblems, gradleOwnerOf, parseBuildDependencies, parseCatalog, parseLockfile } from "./gradle-lock.mjs";

function mem(files) {
  return {
    read: (f) => { if (!(f in files)) throw new Error(`ENOENT ${f}`); return files[f]; },
    exists: (f) => f in files,
    list: (d) => {
      const prefix = d === "." ? "" : `${d}/`; const seen = new Map();
      for (const k of Object.keys(files)) if (k.startsWith(prefix)) { const rest = k.slice(prefix.length); const i = rest.indexOf("/"); seen.set(i < 0 ? rest : rest.slice(0, i), i >= 0); }
      return [...seen].map(([name, isDir]) => ({ name, isDir }));
    },
  };
}
const run = (files, dir = ".") => gradleLockProblems({ dir, ...mem(files) });
const NOT_SET_UP = /Gradle dependency locking is not set up: run \.\/gradlew dependencies --write-locks and commit gradle\.lockfile/;
const CATALOG = `[versions]\ncore = "1.13.1"\n[libraries]\nandroidx-core-ktx = { module = "androidx.core:core-ktx", version.ref = "core" }\nokio = "com.squareup.okio:okio:3.9.0"\njunit = { group = "junit", name = "junit", version = "4.13.2" }\n[bundles]\ntesting = ["junit"]\n`;

test("no lockfile: the dependency locking is not set up", () => {
  const out = run({ "settings.gradle.kts": "", "app/build.gradle.kts": 'dependencies {\n  implementation("com.squareup.okio:okio:3.9.0")\n}\n' });
  assert.equal(out.length, 1);
  assert.match(out[0], /^app\/build\.gradle\.kts: 1 declared dependency and no app\/gradle\.lockfile/);
  assert.match(out[0], NOT_SET_UP);
});

test("matching lockfile passes, literal and catalog dependencies, per-project lockfiles", () => {
  const files = {
    "settings.gradle.kts": "", "gradle/libs.versions.toml": CATALOG,
    "app/build.gradle.kts": 'dependencies {\n  implementation(libs.androidx.core.ktx)\n  implementation("com.squareup.okio:okio:3.9.0")\n  testImplementation(libs.bundles.testing)\n  implementation(project(":lib"))\n}\n',
    "app/gradle.lockfile": "# comment\nandroidx.core:core-ktx:1.13.1=releaseCompileClasspath\ncom.squareup.okio:okio:3.9.0=debugCompileClasspath\njunit:junit:4.13.2=testCompileClasspath\nempty=annotationProcessor\n",
    "lib/build.gradle": "dependencies {\n  api 'com.squareup.okio:okio:3.9.0'\n}\n", "lib/gradle.lockfile": "com.squareup.okio:okio:3.9.0=compileClasspath\n",
  };
  assert.deepEqual(run(files), []);
});

test("a declared direct dependency with no locked version, or a lock below it, fails with file, line and lock", () => {
  const files = {
    "settings.gradle.kts": "", "gradle/libs.versions.toml": CATALOG,
    "app/build.gradle.kts": 'dependencies {\n  implementation(libs.androidx.core.ktx)\n  implementation("com.squareup.okio:okio:3.9.0")\n  implementation("org.example:missing:1.0")\n}\n',
    "app/gradle.lockfile": "androidx.core:core-ktx:1.12.0=x\ncom.squareup.okio:okio:3.9.0=x\n",
  };
  const out = run(files);
  assert.equal(out.length, 2, out.join("\n"));
  assert.match(out[0], /^app\/build\.gradle\.kts:2: androidx\.core:core-ktx:1\.13\.1 \(libs\.androidx\.core\.ktx\) is locked at 1\.12\.0, below the declared 1\.13\.1 in app\/gradle\.lockfile$/);
  assert.match(out[1], /^app\/build\.gradle\.kts:4: org\.example:missing:1\.0 has no locked version in app\/gradle\.lockfile$/);
});

test("a lock above the declared version is fine (conflict resolution); dynamic and interpolated versions need presence only", () => {
  const files = { "settings.gradle": "", "app/build.gradle": "dependencies {\n  implementation 'a:b:1.0'\n  implementation 'c:d:2.+'\n  implementation \"e:f:$ver\"\n  implementation 'g:h'\n}\n", "app/gradle.lockfile": "a:b:1.2=x\nc:d:2.9=x\ne:f:3=x\n" };
  const out = run(files);
  assert.equal(out.length, 1);
  assert.match(out[0], /g:h has no locked version/);
});

test("legacy gradle/dependency-locks per configuration is read", () => {
  const files = { "settings.gradle": "", "app/build.gradle": "dependencies {\n  implementation 'a:b:1.0'\n}\n", "app/gradle/dependency-locks/compileClasspath.lockfile": "a:b:1.0=compileClasspath\n" };
  assert.deepEqual(run(files), []);
});

test("unreadable declarations and catalog problems are findings", () => {
  const files = {
    "settings.gradle.kts": "", "gradle/libs.versions.toml": '[libraries]\nbad = { version = "1" }\nok = "a:b:1.0"\n',
    "app/build.gradle.kts": 'dependencies {\n  implementation(someFunction())\n  implementation(libs.nope)\n  implementation("a:b:1.0")\n}\n', "app/gradle.lockfile": "a:b:1.0=x\n",
  };
  const out = run(files);
  assert.match(out.join("\n"), /gradle\/libs\.versions\.toml:2: library bad has no module or group\/name/);
  assert.match(out.join("\n"), /app\/build\.gradle\.kts:2: implementation dependency cannot be read literally/);
  assert.match(out.join("\n"), /app\/build\.gradle\.kts:3: libs\.nope is not in gradle\/libs\.versions\.toml \[libraries\]/);
});

test("projects with no dependencies need no lock; comments are ignored", () => {
  assert.deepEqual(run({ "settings.gradle.kts": "", "build.gradle.kts": 'plugins { id("x") }\n// implementation("a:b:1.0")\n' }), []);
});

test("helpers: version order, lockfile, catalog, owner", () => {
  assert.ok(compareGradleVersions("1.13.1", "1.2.0") > 0 && compareGradleVersions("1.0", "1.0-rc1") > 0 && compareGradleVersions("1.0.0", "1.0") === 0);
  assert.deepEqual([...parseLockfile("a:b:1.0=x,y\na:b:1.1=z\n").get("a:b")], ["1.0", "1.1"]);
  assert.equal(parseCatalog(CATALOG).libraries.get("androidx-core-ktx").version, "1.13.1");
  assert.equal(parseBuildDependencies('implementation "g:a:1"\nimplementation(platform("g:bom:2"))\n')[1].name, "bom");
  const files = { "x/settings.gradle.kts": "", "x/app/build.gradle.kts": "" };
  const anc = (d) => (d === "." ? ["."] : [".", ...d.split("/").map((_, i, a) => a.slice(0, i + 1).join("/"))]);
  assert.equal(gradleOwnerOf("x/app/build.gradle.kts", { exists: (f) => f in files, ancestorDirs: anc }), "x");
});

test("a catalog alias wrapped in platform(...) is read: implementation(platform(libs.compose.bom)) is the alias compose.bom (hrwin false positive)", () => {
  const deps = parseBuildDependencies("dependencies {\n    implementation(platform(libs.compose.bom))\n    implementation(platform(libs.bundles.compose))\n}\n");
  assert.ok(deps.some((d) => d.kind === "alias" && d.alias === "compose.bom"), JSON.stringify(deps));
  assert.ok(deps.some((d) => d.kind === "bundle" && d.alias === "compose"), JSON.stringify(deps));
  assert.ok(!deps.some((d) => d.kind === "unreadable"), JSON.stringify(deps));
});
