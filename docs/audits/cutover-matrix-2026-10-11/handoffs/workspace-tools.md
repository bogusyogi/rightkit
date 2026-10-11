# Workspace Tools — RightKit migration handoff

Source root: /Volumes/D/claude/tools

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workspace-tools.json)

## @rightkit/theme — N

Remotion production code locally maps brand colors & fonts into component styling; compatible published theme token API is unused.

Adopt @rightkit/theme token application for reusable color/font values; retain brand palette, typography & composition policy.

- Local: [BrandPalette](/Volumes/D/claude/tools/remotion/src/brand-palettes.ts:15) — Local palette owns background, foreground, accent & font values.
- Local: [OutroCard](/Volumes/D/claude/tools/remotion/src/compositions/OutroCard.tsx:39) — Production composition applies local palette to rendered styling.
- Owner: [applyTokens](/Volumes/D/claude/rightkit/packages/theme/src/tokens.ts:26) — Published browser-compatible API applies app-owned CSS color/font token records.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
