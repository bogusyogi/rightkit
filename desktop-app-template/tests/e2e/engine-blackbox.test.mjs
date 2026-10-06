// Engine black box: contract the shell and every headless consumer rely on.
import { scenario, cli, sidecar, scratchAppData, assert } from "./lib.mjs";

scenario("CLI doctor/echo and sidecar agree; clean shutdown on stdin close", async () => {
  const appData = scratchAppData("blackbox");
  const doctor = cli(["doctor"], { appData });
  assert.equal(doctor.status, 0);
  assert.equal(doctor.json.ok, true);
  assert.equal(doctor.json.app_data, appData, "engine honours RIGHTAPP_APP_DATA");

  const s = sidecar(appData);
  const viaSidecar = await s.call("echo", { n: 1 });
  assert.deepEqual(viaSidecar.result, cli(["echo"], { appData, input: { n: 1 } }).json);
  assert.equal((await s.call("nope")).error.code, "unknown_command");
  await s.close();
});

scenario("failures are structured JSON with stable exit codes", async () => {
  const appData = scratchAppData("errors");
  const unknown = cli(["frobnicate"], { appData });
  assert.equal(unknown.status, 3);
  assert.equal(unknown.json.error.code, "unknown_command");
});
