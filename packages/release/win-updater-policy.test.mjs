import test from "node:test";
import assert from "node:assert/strict";
import { FILES_ROWS } from "./admission-files.mjs";
import { validateWinPackage, winPackageUpdaterDisabled } from "./win-package-recipe.mjs";

const recipe = { kind: "winPackage", updater: false };
const raw = { winPackage: { installer: "nsis", productName: "App", updater: false, rawExe: "app.exe", installerFile: "setup.exe" } };
const row = FILES_ROWS.find((item) => item.id === "win-updater-artifacts");
const target = { signingContract: "windows-raw-exe-authenticode-before-nsis-v1", recipe };

test("only an explicitly disabled expanded recipe skips updater artifacts", () => {
  assert.equal(winPackageUpdaterDisabled(raw, target), true);
  assert.equal(row.run({ platform: "win", rawTarget: raw, target }), undefined);
  for (const rawTarget of [{}, { recipe }, { winPackage: { updater: true } }]) {
    assert.equal(winPackageUpdaterDisabled(rawTarget, target), false);
    assert.match(row.run({ platform: "win", rawTarget, target })[0], /updater\.artifacts/);
  }
  assert.equal(winPackageUpdaterDisabled(raw, { ...target, recipe: { ...recipe, updater: true } }), false);
});

test("disabled updater refuses declared artifacts", () => {
  assert.throws(() => validateWinPackage({ appRoot: process.cwd(), target: { ...raw, updater: { artifacts: [{ file: "setup.exe" }] } } }), /conflicts with updater\.artifacts/);
  assert.equal(validateWinPackage({ appRoot: process.cwd(), target: raw }).updater, false);
  assert.equal(validateWinPackage({ appRoot: process.cwd(), target: { ...raw, updater: { artifacts: [] } } }).updater, false);
  for (const artifacts of [null, {}, "bad"]) {
    assert.throws(() => validateWinPackage({ appRoot: process.cwd(), target: { ...raw, updater: { artifacts } } }), /artifacts must be an array/);
  }
  for (const [artifacts, pattern] of [["bad", /artifacts must be an array/], [[{ file: "setup.exe" }], /conflicts with updater\.artifacts/]]) {
    const updater = Object.create({ artifacts });
    assert.throws(() => validateWinPackage({ appRoot: process.cwd(), target: { ...raw, updater } }), pattern);
  }
});
