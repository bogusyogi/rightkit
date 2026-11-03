# @rightkit/send-ui npm consumer (npm.@rightkit/send-ui)

Packed-archive consumer for `@rightkit/send-ui` 0.1.0 (the workspace version). The unit is installed only from the sealed `file:` archive. The reviewed closure pins `react` 19.3.1, the unit's only peer, and `react-dom` 19.3.1 for `renderToStaticMarkup` in plain Node. No browser, no DOM library.

Surface (exact `exports` keys): `.` and `./send-ui.css`. The consumer asserts the sorted export names of the root entry (`SendPanel`, `createSendClient`, `defaultWords`, `isSendError`, `useSend`), the manifest (`sideEffects` `*.css`, `react` as the only peer, no dependencies) and reads the shipped stylesheet through the archive-bound export URL.

Scenarios (5): export surface and shipped stylesheet; the default words (every string present, the six transfer states, the singular and plural file count, the typed-error wording by code and the generic fallback); the typed-error guard; `createSendClient` mapping every one of its thirteen calls to the one JSON command with the documented argument objects, returning the host's reply unchanged, copying the send request and passing a host-thrown typed error through; and the `SendPanel` server markup.

What it proves: the wire contract of the JSON command surface (command names and argument shapes) as a host would drive it, and the panel's loading markup. Server rendering runs no effect, so `SendPanel` renders its busy placeholder (`aria-busy="true"`, `className` override honoured) and issues no command.

What it does not prove: anything that needs a mounted tree. The loaded panel (devices, incoming requests, transfers, the trust list), subscription and polling, the file picker, button and select handling and error display are client effects and are not run; CSS is checked as text only. No `rightkit-send` crate is run: the host's replies are plain synthetic objects, so nothing here claims the crate's real snapshot content, a real transfer or a real device.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). The runner provides them. `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive.

All paths, fingerprints and names are synthetic. No network, no credentials, no user data. Output is one JSON line; exit 0 only when all five scenarios pass. `nativeHostQualified` is always false.
