# Changelog

## 0.1.0 - 2026-10-17

Initial version (rightkit#264).

- `beats`: bracketed-script parsing and snapping to `rightkit-transcript` word timings, with typed errors.
- `sheet`: inspect-sheet planning with the `rightkit.motion.sheets.v1` manifest; defaults (2x3 grid, 960x540 tiles, 480 px minimum tile width) are to confirm. Feature `assemble` composes PNG sheets.
- The `take` module is not built; it stays gated until a producer is named.
- Written without a compile; CI is the first compiler.
