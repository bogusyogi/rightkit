# @rightkit/doc-render npm consumer (draft, npm.@rightkit/doc-render)

Packed-archive consumer for `@rightkit/doc-render` 0.2.1 (workspace version). The unit is installed only from the
sealed `file:` archive. The reviewed closure pins every direct registry dependency and the React peer to the exact
version recorded in `pnpm-lock.yaml` (importer `packages/doc-render`). No `@rightkit/*` package is in the closure.

Surface (exact `exports` keys): `.`, `./shiki.css`, `./styles.css`.

Scenarios (6): root export surface; raw HTML never emitted as a script; link policy allows only reviewed schemes;
parse tree deterministic with a clearable cache; stylesheet subpaths bound to the archive.

No network, no user data. Markdown inputs are inline fixtures. Writes nothing outside the scratch install.
