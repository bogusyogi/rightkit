# @rightkit/legal-ui npm consumer (npm.@rightkit/legal-ui)

Packed-archive consumer for `@rightkit/legal-ui` 0.4.1 (the workspace version). The unit is installed only from the sealed `file:` archive. The reviewed closure pins `react` 19.3.1, the unit's only peer, and `react-dom` 19.3.1 for `renderToStaticMarkup` in plain Node. No browser, no DOM library.

Surface (exact `exports` keys): `.` and `./legal-ui.css`. The consumer asserts the sorted export names of the root entry (`LegalDocumentBody`, `LegalGate`, `policyGateDocuments`) and reads the shipped stylesheet through the archive-bound export URL.

Scenarios (7): export surface and shipped stylesheet; `policyGateDocuments` mapping; Markdown rendering of `LegalDocumentBody` (legacy mode: heading levels, lists, rules, emphasis, code, safe links only, escaping, title omission, CRLF); the extended mode and the text format; the first-run `LegalGate` default markup; the re-acceptance variant with every copy override; theme tokens, layout, lockup and logo options and the wordmark split.

What it proves: the gate renders the documented structure for the supplied props. The form is named by its heading and trust line; hosted documents link out in a new window, bundled documents render as collapsed inline disclosures and `onOpen` documents link to the host; the eligibility radios and the agreement checkbox start unchecked; the Agree button starts disabled; the default trading-name notice and publisher lockup are present; `layout="contained"` drops the viewport minimum height; and the gate never sets `data-theme` or `color-scheme`. The Markdown renderer produces only `h2` to `h6`, lists, `hr`, `strong`, `em`, `code` and `http(s)` or `mailto` links, and escapes everything else.

What it does not prove: anything that needs a mounted tree. Choosing an eligibility basis, ticking the agreement, pressing Agree or Quit, the `onAgree` payload (`version`, `acceptedAt`, `manifestSha256`, `eligibilityBasis`), the busy state, the error alert and retry after a rejected save, `onError`, opening a bundled document, the runtime style injection and the Tanker font registration are client effects and are not run. CSS is checked as text only. `@rightkit/legal` is not installed; the mapping helper is fed plain objects of the structural type it documents.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). The runner provides them. `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive.

All URLs use the reserved `.invalid` TLD and are never fetched. No network, no credentials, no user data. Output is one JSON line; exit 0 only when all seven scenarios pass. `nativeHostQualified` is always false.
