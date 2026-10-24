# @rightkit/hooks npm consumer (draft, npm.@rightkit/hooks)

Packed-archive consumer for `@rightkit/hooks` 0.1.2 (workspace version). Recipe ID: `hooks-normalize-dispatch-bin-consumer`. The unit is installed only from the sealed `file:` archive. The reviewed closure has no dependencies: the package declares none and imports only Node built-ins.

Surface (exact `exports` keys): `.`, which resolves to `lib/runtime.mjs`. The package also ships the `rightkit-hooks` bin at `bin/rightkit-hooks.mjs`. The consumer runs the installed bin file with Node (`process.execPath`) and never uses a `.bin` shim, so it behaves the same under pnpm on Windows and macOS.

Scenarios (8):

1. Installed manifest bound to the archive (name and version, `type: module`, the single `.` export, the `rightkit-hooks` bin, no dependencies, a Node shebang) and the exact module export surface, `dispatchHookEvent` and `normalizeHookEvent`.
2. `normalizeHookEvent`: the three spellings of the event, session, tool and tool-use fields and the order they are tried in, skipped blank and non-string values, `null` for a missing field, a frozen result with a copied payload, and refusal of a non-object payload or a missing event.
3. `dispatchHookEvent`: modules run one after another in declared order (sync and async), the exact result shape, `output: null` for a module that returns nothing, a throwing or rejecting module reported as an `error` result without stopping the next one, a frozen result, and an event no module can mutate.
4. The per-module deadline: a module that never settles gets a typed timeout error naming it, its `AbortSignal` is aborted, the next module still runs, and a module that finishes in time is not aborted later.
5. Argument validation: modules must be an array, `timeoutMs` must be a positive integer, the payload must have an event, and a module without a string `id` and a `handle` function rejects the dispatch.
6. The bin's exit codes and output: `64` with the usage line and no output when no module is given, `0` and one JSON line for an ok result, `1` for a non-ok result with the result still printed, several modules in order, and a module without an `id` export named by the path it was given.
7. The bin's input boundaries: UTF-8 split across chunks is reassembled, exactly 1 MiB of stdin is read, one byte more is refused while reading (before any module loads), and malformed JSON, empty input, a non-object and a payload without an event each exit `2` with one `RIGHTKIT_HOOKS_INVALID:` line on stderr and nothing on stdout.
8. Module loading: a module path that does not exist and a module that throws when imported each exit `2` with nothing on stdout, and no module before the failing one runs.

Hook modules for the bin scenarios are written to a private temporary directory under the OS temp directory and removed at the end; nothing is written inside the install. About sixteen short child processes of Node are started. No network, no credentials, no user data. The longest wait is a 60 ms pause that proves a finished module is not aborted late.

Not covered: `rightkit-hooks` used through a package-manager `.bin` shim (`pnpm exec`), which depends on the host shell; the default 3000 ms deadline (the consumer passes shorter ones); and the hook systems that invoke the bin.
