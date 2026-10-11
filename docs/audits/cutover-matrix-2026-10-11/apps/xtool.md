# Xtool — fresh cutover review

Reviewed current TypeScript Node host, direct Swift macOS helper, packaging scripts & production manifests at `17dcc15c67b43464850ed24f5d36b096ebdc9f4f`. Xtool has no `@rightkit/*` dependency, Cargo target, SwiftPM dependency, Rust FFI, child-service bridge, Tauri host, or browser/React surface.

All 97 catalog rows are `NA`: 57 Rust owners lack a usable Node bridge; 15 Swift modules own features absent from Xtool's narrow desktop-control helper; 25 npm owners either require an absent host/surface or provide a different feature. No production caller routes through RightKit, so there are no Y/M rows. Xtool's local logging, document codecs, secrets, search, MCP server, desktop control & packaging remain app-owned Node mechanisms rather than evidence of compatible shared-owner duplication.

No direct cutover is supported by current compatible hosts. Reuse opportunities remain for Node MCP/HTTP/process/credential/desktop-control code if RightKit adds a supported Node binding or packaged sidecar; these are shared-platform expansion work, not completed migrations. `delivery_flags` records no local-path or workspace RightKit dependency; audit scope is source only, with no execution qualification.
