# Right Suite release pipeline: target contract

This file holds the bounded target contract that rightkit#397 ("dev hot-reload loop" and "linux target") needed. The command surface is in
`packages/release/README.md`; the gate is in `docs/GATE-CONTRACT.md`; the pipeline stages are in `docs/PIPELINE-ARCHITECTURE.md`. Read them for
everything else.

## Targets

| Target | Dev lane (`right-release dev`) | Release lane (`right-release build`, `publish`) |
|---|---|---|
| macOS | Tauri DevID or assembled app: built, signed with the product identity, installed in place | signed, notarized, sealed |
| Windows | Tauri Authenticode or NSIS install, assembled app: built, signed, installed in place | signed, sealed |
| iOS, Android | device or simulator install | TestFlight, Play internal track |
| Linux | `development.targets.linux`: build, and (with `archive`) one tarball per architecture plus a digest manifest a guest link pins (rightkit#1230), from a clean checkout only | **none** |

**Linux is not a release target.** Adrian, 2026-11-03: "linux is the wrong target and it has to be only where we have brokers i.e. mac and windows".
A Linux host or hosted CI runner is a throwaway container (qualification and the dev archive), never a shipped, signed or published product. The
catalog and planner may name `*-unknown-linux-gnu` triples; no `targets.linux` release recipe, signing path or upload exists, and none is added
without a new written decision from Adrian.

## Dev loop

**There is no separate hot-reload loop.** GATE-CONTRACT GC-A07 (Adrian, 2026-11-02): "Dev is the installed app; there is no debug copy; QA journeys
run after the build." The development loop is `right-release dev --platform <mac|win>`: the release Cargo profile plus `qa-native`, built through the
gate, signed with the product identity and installed over the previous copy at the same path and id. QA runs against that installed copy, never a
dev server. So the release tooling does not start a Vite dev server, does not hand a `TAURI_CONFIG` to `build.rs`, and does not watch files; an
app's own `pnpm dev` is the app's, outside the gate, and proves nothing about a build, QA or a release.

What the dev lane proves about itself after the install (rightkit#397, #1480; see `packages/release/README.md`, "Release switch scan"):

1. the installed dev binary carries the QA control marker (and any `releaseSwitches.devSelfCheck.expectPresent` name), so the release scan's deny
   strings are proven visible (`dev-expect-absent`);
2. the web bundle and the native binary agree about the QA surface: `__RIGHTKIT_QA__` (true only when `RIGHTKIT_BUILD_MODE=dev`) and `qa-native`
   are built together (`web-qa-missing`, `web-qa-unpaired`).

The release lane refuses both halves: no QA control marker, no `RIGHTKIT-QA-WEB-COMPILED`, no `__RIGHTKIT_QA__`, no retired per-app QA flag.
