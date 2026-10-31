#!/usr/bin/env node
// `node qa-journeys-prebuilt-run.mjs <rightkit-qa> -- journeys run <args>`: one `rightkit-qa journeys run` step of a snapshot stage (GB-79, capability
// `journeys-prebuilt-input`) as the gate's build-once route (qa-journeys-prebuilt.mjs): carry the build, sign the products, hand QA the verified input.
// build-launch.mjs hands such a step to this entry instead of refusing it.
import { carriedCargoMapOf } from "./carried-cargo.mjs";
import { runJourneysPrebuilt } from "./qa-journeys-prebuilt.mjs";

const stop = (message, status = 101) => { process.stderr.write(`rightkit-gate: ${message}\n`); process.exit(status); };
const [qa, separator, ...args] = process.argv.slice(2);
if (!qa || separator !== "--") stop("RK_QA_JOURNEYS_USAGE: usage: qa-journeys-prebuilt-run.mjs <rightkit-qa> -- journeys run <args>");
const carried = carriedCargoMapOf(process.env);
if (!carried?.map) stop(`RK_QA_JOURNEYS_CARRIER_MISSING: ${carried?.error ?? "no carried map in the environment"}; a snapshot stage's build is never run around its carrier`);
try {
  const { status } = runJourneysPrebuilt({ qa, args, map: carried.map });
  process.exit(status);
} catch (error) {
  stop(String(error.message ?? error).startsWith("RK_") ? error.message : `${error.code ?? "RK_QA_JOURNEYS_FAILED"}: ${error.message}`, Number.isInteger(error.status) ? error.status : 101);
}
