import assert from "node:assert/strict";
import { test } from "vitest";
import {
  classifyConnectionError,
  isAuthRejectionError,
  isGatedMissingHealthError,
  isMissingHealthEndpointError,
  isStaleConnectionError,
  projectConnectionError,
} from "./connectionErrorProjection";

const GATE_401 = '401: {"reason":"no_cookie"}';

test("projects a local reachability failure into local recovery", () => {
  const projection = projectConnectionError(new Error("ECONNREFUSED"), "local");
  assert.equal(projection.kind, "unreachable");
  assert.equal(projection.recoveryVariant, "local-unavailable");
  assert.equal(projection.retryable, true);
});

test("projects missing, service, and compatibility failures separately", () => {
  assert.equal(projectConnectionError(new Error("404: Not Found"), "remote").kind, "missing");
  assert.equal(projectConnectionError(new Error("404: wrong service"), "remote").kind, "wrong-service");
  assert.equal(projectConnectionError(new Error("wrong service at host"), "remote").kind, "wrong-service");
  assert.equal(projectConnectionError(new Error("426: update required"), "remote").kind, "incompatible");
  assert.equal(projectConnectionError(new Error("426: update required"), "remote").recoveryVariant, "remote-incompatible");
});

test("auth failures are actionable without exposing credentials", () => {
  const projection = projectConnectionError(
    new Error("401: https://user:secret@example.com/api?token=top-secret"),
    "remote",
  );
  assert.equal(projection.kind, "unauthorized");
  assert.equal(projection.retryable, false);
  assert.equal(projection.detail.includes("top-secret"), false);
  assert.equal(projection.detail.includes("secret"), false);
});

test("superseded attempts are non-recovering stale results", () => {
  const error = Object.assign(new Error("SSH bootstrap was superseded by newer connection settings."), {
    kind: "superseded",
  });
  const projection = classifyConnectionError(error, "remote");
  assert.equal(projection.stale, true);
  assert.equal(projection.recoveryVariant, null);
  assert.equal(projection.retryable, false);
  assert.equal(isStaleConnectionError(error), true);
});

test("health error predicates preserve the donor's missing-route distinctions", () => {
  assert.equal(isMissingHealthEndpointError(new Error("404: Not Found")), true);
  assert.equal(isMissingHealthEndpointError(new Error("500: boom")), false);
  assert.equal(isAuthRejectionError(new Error("403: Forbidden")), true);
  assert.equal(isGatedMissingHealthError(new Error(GATE_401)), true);
  assert.equal(isGatedMissingHealthError(new Error("401: Unauthorized")), false);
});
