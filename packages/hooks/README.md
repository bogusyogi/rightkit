# @rightkit/hooks

Neutral lifecycle runtime for agent hook events. HookHost normalizes a hook payload, runs the configured modules in order under a deadline, and returns a typed result. It grants no authority and contains no policy.

| Item | Value |
|---|---|
| Where it runs | Dev tooling (Node process invoked by an agent hook system) |
| Rust pair | `rightkit-hooks` (crate) |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/hooks` | `lib/runtime.mjs`: `normalizeHookEvent(payload)`, `dispatchHookEvent(payload, modules, { timeoutMs })` (default 3000 ms per module) |
| `rightkit-hooks` (bin) | Reads one JSON payload from stdin and dispatches it to the module files given as arguments |

A module file exports `handleHook` and may export `id`.

Bin exit codes: `0` on an ok result, `1` on a non-ok result, `2` on invalid input (including stdin over 1 MiB) or a module that fails to import, `64` on usage errors.

Peer dependencies: none.

## Example

```js
import { dispatchHookEvent } from "@rightkit/hooks";

const result = await dispatchHookEvent(payload, [{ id: "guard", handle: handleHook }]);
```

```sh
pnpm exec rightkit-hooks ./guard.mjs < payload.json
```

## Known issues

- None recorded in this repository. `rightkit-ownership.json` still notes "stdin repair remains P1"; its status is not verified here.
