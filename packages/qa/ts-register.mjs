// Test-only: `node --import @rightkit/qa/ts-register --test ...` lets `node --test` run TypeScript sources that
// import extensionless and import JSON without attributes (Vite resolves both). Node strips the types itself;
// this hook only fills in the resolution.
import { register } from "node:module";
register("./ts-hooks.mjs", import.meta.url);
