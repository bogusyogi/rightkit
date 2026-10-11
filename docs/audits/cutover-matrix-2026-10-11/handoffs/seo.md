# SEO — RightKit migration handoff

Source root: /Volumes/D/claude/workflows/seo

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo.json)

## @rightkit/qa-chrome — N

SEO's standalone Node render-gap CLI directly discovers Chrome/Edge, spawns it with a profile & CDP port, then kills it. @rightkit/qa-chrome exposes a Node-compatible Chrome-for-Testing resolver plus launch/close API, but no SEO caller imports it; overlap is browser discovery/lifecycle only, not raw CDP signal comparison.

Optional: if a Node package dependency is intentionally adopted, replace only render-gap browser discovery/lifecycle with @rightkit/qa-chrome; retain SEO raw-CDP comparison logic.

- Local: [findBrowser](/Volumes/D/claude/workflows/seo/scripts/render_gap.mjs:146) — Local Node CLI resolves Chrome/Edge from OS paths.
- Local: [main](/Volumes/D/claude/workflows/seo/scripts/render_gap.mjs:241) — Local Node CLI spawns browser with profile & remote-debugging port.
- Local: [main](/Volumes/D/claude/workflows/seo/scripts/render_gap.mjs:266) — Local cleanup closes socket then kills browser process.
- Owner: [resolveChromeForTesting](/Volumes/D/claude/rightkit/packages/qa-chrome/src/index.ts:125) — Owner exports a Node callable resolver for Chrome for Testing.
- Owner: [launchChromeForTesting](/Volumes/D/claude/rightkit/packages/qa-chrome/src/index.ts:202) — Owner exports Node launch/profile/CDP lifecycle with idempotent graceful close.

Delivery: Node is directly usable by the standalone helper; @rightkit/qa-chrome is not installed or imported by current SEO source.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
