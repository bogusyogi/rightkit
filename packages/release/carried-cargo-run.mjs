#!/usr/bin/env node
// `node carried-cargo-run.mjs <cargo args>`: one Cargo call of a snapshot stage on a broker host, carried (carried-cargo.mjs) from
// this process's working directory. build-launch.mjs hands a configured `cargo` or `rustright cargo` step to this entry instead of
// to the RustRight client, so a config step is carried the same way on every platform (Windows included: no .cmd or shim is involved).
import { runCargoSync } from "./managed-cargo.mjs";

const result = runCargoSync(process.argv.slice(2), { cwd: process.cwd(), env: process.env, stdio: "inherit", windowsHide: true });
if (result.error) process.stderr.write(`rightkit-gate: ${result.error.message}\n`);
process.exit(result.status ?? 1);
