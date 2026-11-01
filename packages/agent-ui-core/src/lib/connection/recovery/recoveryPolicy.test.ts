import assert from "node:assert/strict";
import { describe, test } from "vitest";
import {
  getRecoveryConfig,
  resolveRecoveryNextStep,
  type RecoveryNextStep,
  type RecoveryPrimaryAction,
  type RecoveryVariant,
} from "./recoveryPolicy";

const EXPECTED_ROUTING: Record<RecoveryVariant, Record<RecoveryPrimaryAction, RecoveryNextStep["kind"]>> = {
  "local-unavailable": { "use-local": "local-setup", "use-remote": "remote-form" },
  "remote-unreachable": { "use-local": "switch-default-to-local", "use-remote": "remote-form" },
  "remote-incompatible": { "use-local": "switch-default-to-local", "use-remote": "remote-form" },
  "remote-wrong-service": { "use-local": "switch-default-to-local", "use-remote": "remote-form" },
  "remote-missing": { "use-local": "switch-default-to-local", "use-remote": "remote-form" },
  "missing-default-host": { "use-local": "switch-default-to-local", "use-remote": "remote-form" },
};

describe("resolveRecoveryNextStep", () => {
  for (const [variant, actions] of Object.entries(EXPECTED_ROUTING) as [
    RecoveryVariant,
    Record<RecoveryPrimaryAction, RecoveryNextStep["kind"]>,
  ][]) {
    for (const [action, expectedKind] of Object.entries(actions) as [
      RecoveryPrimaryAction,
      RecoveryNextStep["kind"],
    ][]) {
      test(`${variant} + ${action} -> ${expectedKind}`, () => {
        assert.deepEqual(resolveRecoveryNextStep(variant, action), { kind: expectedKind });
      });
    }
  }
});

test("local-unavailable exposes both actions and local retry", () => {
  const config = getRecoveryConfig("local-unavailable");
  assert.equal(config.iconKey, "local");
  assert.equal(config.showRetry, true);
  assert.equal(config.retryLabel, "Retry Local");
  assert.equal(config.showUseLocal, true);
  assert.equal(config.showUseRemote, true);
});

test("remote-unreachable exposes both actions and retry", () => {
  const config = getRecoveryConfig("remote-unreachable", "My Server", "https://example.com:4096");
  assert.equal(config.iconKey, "remote");
  assert.equal(config.showRetry, true);
  assert.equal(config.useLocalLabel, "Use Local");
  assert.equal(config.useRemoteLabel, "Use Remote");
});

test("wrong-service exposes both actions without retry", () => {
  const config = getRecoveryConfig("remote-wrong-service", "Bad Host", "https://wrong.example.com");
  assert.equal(config.showRetry, false);
  assert.equal(config.showUseLocal, true);
  assert.equal(config.showUseRemote, true);
});

test("remote descriptions redact query strings and credentials", () => {
  const config = getRecoveryConfig(
    "remote-unreachable",
    "https://admin:s3cret@example.com:4096?token=super-secret&auth=abc123",
  );
  assert.equal(config.description.includes("s3cret"), false);
  assert.equal(config.description.includes("super-secret"), false);
  assert.equal(config.description.includes("REDACTED"), true);
  assert.equal(config.description.includes("example.com"), true);
});

test("local and chooser descriptions ignore host context", () => {
  const host = "https://example.com?token=secret";
  assert.equal(getRecoveryConfig("local-unavailable", "Some Host", host).description.includes("example.com"), false);
  assert.equal(getRecoveryConfig("missing-default-host", "Some Host", host).description.includes("example.com"), false);
});
