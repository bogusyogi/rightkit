# LightCraft cutover matrix

Current source at `fd6a998949072a5343e982cb99bdbe5815433cc5` exposes native egui desktop, Rust CLI/MCP & Rust/WASM web. It has no active RightKit route.

- 97 catalog rows: 8 N, 89 NA, 0 Y, 0 M.
- Photo/raw/rendering algorithms, photo-specific overlays & domain schemas remain LightCraft-owned.

## Concrete gaps

| Component | Target | Current evidence | Cutover task |
|---|---|---|---|
| `cargo:rightkit-cache` | `/Volumes/D/claude/workflows/lightcraft/crates/preview/src/disk.rs:20` | Native & web thumbnail/source caches use LightCraft DiskCache/Lru; no RightKit cache route. | Adapt rightkit-cache for native cache metadata only; retain LightCraft image encoding, keys & web OPFS policy. |
| `cargo:rightkit-crash` | `/Volumes/D/claude/workflows/lightcraft/crates/engine/src/guard.rs:21` | Native command guard & panic hook are local; no RightKit crash capture route. | Wire shared crash capture around existing command guard; retain LightCraft recovery semantics. |
| `cargo:rightkit-framed-sidecar` | `/Volumes/D/claude/workflows/lightcraft/crates/mcp/src/server.rs:36` | LightCraft MCP owns newline JSON-RPC framing without RightKit bounded-frame transport. | Adopt bounded framing below LightCraft MCP schema; retain all tool/resource semantics. |
| `cargo:rightkit-fs` | `/Volumes/D/claude/workflows/lightcraft/crates/catalog/src/safe_file.rs:153` | LightCraft implements atomic durable writes & library locks locally; no RightKit fs route. | Map native catalog/sidecar persistence to rightkit-fs; retain photo-specific durability classes & web store. |
| `cargo:rightkit-mcp` | `/Volumes/D/claude/workflows/lightcraft/crates/mcp/src/server.rs:36` | CLI exposes a hand-written LightCraft MCP server over stdio; no shared MCP host route. | Adapt LightCraft tool catalog to rightkit-mcp server host; retain photo command schema & resources. |
| `cargo:rightkit-qa` | `/Volumes/D/claude/workflows/lightcraft/crates/ui-egui/src/headless.rs:105` | Custom CI plus deterministic headless egui driver remain local; no RightKit QA harness route. | Use rightkit-qa around control/CLI qualification; retain headless renderer & image assertions. |
| `cargo:rightkit-search` | `/Volumes/D/claude/workflows/lightcraft/crates/catalog/src/query.rs:19` | Catalog owns live free-text/filter query evaluation; no RightKit query route. | Introduce rightkit-search only for reusable text/operator parsing; retain photo filter rules & catalog ranking. |
| `cargo:rightkit-settings` | `/Volumes/D/claude/workflows/lightcraft/crates/engine/src/library.rs:201` | Library preference files have local corruption preservation, load & save flow; no RightKit settings route. | Adapt rightkit-settings behind library preference files; retain library-scoped schemas & corruption UX. |

No npm/React host exists: web UI is Rust/WASM with only worker glue. No Swift target exists. Owner APIs were checked for every N; no owner route is live, so Y/M do not apply.

HTTP, LLM, model-install, durable diagnostics, FFmpeg media/playback & managed-child owners are NA: source has no matching production subsystem. LightCraft's image UI & theme remain egui/Rust; `@rightkit/media-ui` & `@rightkit/theme` have no compatible npm/React host.

