// `test` for the files that start the installed RustRight broker (test-support/real-broker.mjs). Not shipped: lives under test-support/.
//
// A Linux direct origin (GC-T04: no broker by default, rightkit#1317; GitHub's qualify-direct runner) has no installed RustRight, so a test that needs the real broker cannot run
// there. Such a test is recorded as SKIPPED with the reason, exactly like the sibling-checkout tests (right-suite-contract.test.mjs): the unit is named in allow_missing_assets
// (ci/asset-gated.json, ci/rk-pipeline.json) and the receipts keep the skipped count. The broker is never simulated, and RIGHTKIT_REQUIRE_ASSETS=1 turns the skip back into a failure.
// Mac, Dell and any host with `rustright` on PATH run these tests against the installed broker as always: there the absence of a broker is still a failure.
import nodeTest from "node:test";
import { BROKER_HOST_ABSENT } from "./real-broker.mjs";

function wrap(fn) {
  if (typeof fn !== "function") return fn;
  return async function brokerAware(t, ...rest) {
    try {
      return await fn.call(this, t, ...rest);
    } catch (error) {
      if (error?.code === BROKER_HOST_ABSENT && process.platform === "linux" && process.env.RIGHTKIT_REQUIRE_ASSETS !== "1") {
        t.skip(error.message);
        return undefined;
      }
      throw error;
    }
  };
}

function test(...args) {
  const last = args.length - 1;
  if (last >= 0 && typeof args[last] === "function") args[last] = wrap(args[last]);
  return nodeTest(...args);
}

export default test;
