import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { developmentEnvironment, resolveCommand } from "./development.mjs";
import { managedRuntimeClient } from "./managed-cargo.mjs";

// Regression (0.2.117): dev builds ran the managed client directly and lost the broker endpoint the
// agent-bin shim defaults, so `rightkit rustc -vV` failed with "broker endpoint is not configured".
const client = managedRuntimeClient({});
const socket = client && path.join(path.dirname(path.dirname(path.dirname(path.dirname(client)))), "broker.sock");
const managed = Boolean(client && existsSync(socket));

test("development environment defaults the broker socket like the agent-bin shim", { skip: !managed && "no managed RightKit host" }, () => {
  assert.equal(developmentEnvironment({ PATH: "/usr/bin" }).RIGHTKIT_BUILD_BROKER_SOCKET, socket);
  assert.equal(developmentEnvironment({ PATH: "/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/explicit.sock" }).RIGHTKIT_BUILD_BROKER_SOCKET, "/explicit.sock");
});

test("resolveCommand('rightkit') hands the child the broker socket when the caller has none", { skip: !managed && "no managed RightKit host" }, () => {
  const launch = resolveCommand("rightkit", ["rustc", "-vV"], { env: { PATH: "/usr/bin" } });
  assert.equal(launch.env?.RIGHTKIT_BUILD_BROKER_SOCKET, socket);
  assert.equal(resolveCommand("rightkit", ["rustc"], { env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/x.sock" } }).env, undefined);
});
