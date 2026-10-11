# Legion cutover audit

97/97 catalog rows reviewed from live source at `3e280ef7ec92f89ee97e492d70295b92032790a3`.

- **Y: 1** — `@rightkit/release` is invoked by native xtask bridge for archive, SBOM/provenance & release-finalization work.
- **N: 11** — direct local implementations remain for browser automation, SQLite bootstrap, HTTP, MCP server host, OAuth, child-process containment, browser QA, sandboxing, OAuth secret storage, AX portable-core & macOS artifact installation.
- **M: 0** — no row showed both shared production route & a competing local implementation in same owner scope.
- **NA: 85** — remaining owner capabilities have no active Legion target or contract overlap. CI compiles Cocoa installer; diagnostics, overlays & UI are reviewed against actual scopes.

Concrete cutovers: use `rightkit-browser` for visual scripts; `rightkit-db` below decision schema; `rightkit-http` + `rightkit-oauth` + `rightkit-secrets` for Google auth; `rightkit-mcp` for stdio host; `rightkit-process` + `rightkit-sandbox` for effects; `rightkit-qa` for browser QA; replace local AX portable-core port with versioned `@rightkit/ax` bridge. For macOS installer, package payload as SHA-256-pinned single-root `.tar.gz`, map version/current layout & use `RightKitArtifacts` validation/recovery.

Review excluded generated artifacts, tests, fixtures, docs, prior audits, builds, installs & network.
