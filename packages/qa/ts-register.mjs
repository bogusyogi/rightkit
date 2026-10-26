// Test-only: `node --import @rightkit/qa/ts-register --test ...` lets `node --test` run TypeScript sources that
// import extensionless and import JSON without attributes (Vite resolves both). Node strips the types itself;
// this hook only fills in the resolution. Registered with `module.registerHooks()` (synchronous, in-thread):
// `module.register()` is deprecated on Node 26 (DEP0205) and its warning made importing this module noisy, which the
// exports import-effects check refuses (rightkit#1193).
import { registerHooks } from "node:module";
import { hooks } from "./ts-hooks.mjs";
registerHooks(hooks);
