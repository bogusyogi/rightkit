# RightKit crates on Linux: state at hand-off, 2026-11-10 22:20 UTC

Written by fabdell (Claude, Dell) for whoever continues on a Linux cloud box. Adrian, 2026-11-10: "i need to move this work remote which means rightkit crates need to be buildable via the gate on linux".

## What is proven

- **Linux, through the gate, green.** Dell WSL2 (Ubuntu 24.04, Node 26.12.1, pnpm 12.16.1, cargo 1.99.1), commit `09aaf01e1`, units `cargo:rightkit-crypto` and `cargo:rightkit-time`: gate exit 0, 2 passed of 2, 4 min 34 s. Quick checks 1,156 of 1,156, release-tier check 62 rules, compile-tier check 14 rules, 18 compile rows (host lint, macOS and Windows cross lint, docs, tests).
- **Windows, through gate and broker, green.** Dell, commit `09aaf01e1`: 46 passed of 46 (the 45 patch-bumped crates and `rightkit-tts-qwen-sys` 0.1.5). Warm run 17.6 min.
- `origin/main` carries all of it. RustRight `origin/main` is `18c1f534`.

- **Linux cloud container, through the gate, 45 of the 46 green (2026-11-11).** A fresh Ubuntu 24.04 container prepared only by `scripts/linux-cloud-setup.sh` (4 cores, 15 GB), commit `fb11f21df`: gate exit 0, 45 passed of 45, 726 compile rows, 14 min 35 s warm. The 46th, `rightkit-tts-qwen-sys`, builds only for macOS arm64 and Windows MSVC (checked-in bindings), so it is scoped off Linux in `ci/gate.config.json`; a Linux unit list that names it is refused ("no executable publisher compile requirements").
- What that run needed and now has: the setup script installs pnpm and the workspace before the setup command, fills the pnpm store without build scripts, reinstalls after the store fill, installs the GTK and WebKit system libraries, and can be run twice; rustdoc links in `rightkit-daemon`, `rightkit-credentials`, `rightkit-agent-tools` and `rightkit-bench` that failed with warnings denied are fixed. All on branch `claude/busy-hypatia-e30h7l`, not on `main`.

## What is not proven

- The Linux run above is on the branch, not on `main`; `main` still has the five failures it fixed.
- On Linux the gate runs the older per-crate path (direct, no broker). The build-together path is used only on the Mac and the Dell.
- A Linux run cannot publish: a direct host writes its result but dispatch publication is held (`RK_DIRECT_RELEASE_HELD`, `packages/release/unit-gate.mjs`). No signing and no macOS or Windows builds on Linux; it lints those targets only.

## Commands

Prepare a box (once):

```bash
bash scripts/linux-cloud-setup.sh
```

Compile a list of crates:

```bash
node packages/release/bin/rightkit-gate.mjs --commit <full sha> --to compile --units-file units.json --receipt-dir <directory that does not exist yet>
```

`units.json` is `{"units":["cargo:rightkit-crypto","cargo:rightkit-time"]}`. The commit must exist in the checkout. The gate builds from a snapshot of the commit, not from the working files.

## What the first runs on a new box hit, in order

1. `actionlint` not enrolled: the quick check `actionlint` fails with "required tool actionlint is missing". Step 5 of the setup script.
2. `RK_CROSS_CC_UNPROVISIONED`: the zig C toolchain is missing. Step 6.

## Open work, not started or not finished

- **Crate publish.** The 46 crates are staged on the Mac (`~/scratch/rk-publish-09aaf01e1/publish.sh`, pass files from the Dell). It needs the Mac's desktop session for the crates.io token and Adrian's yes. Nothing is published.
- **npm.** 24 packages have a version in the repository that is not on the registry, `@rightkit/release` 0.6.53 among them. Not published.
- **Broker fix on the Dell.** `18c1f534` is signed and staged, the cutover plan passed, not applied. It stops the 35 s OpenSSL step that fails on three jobs per run.
- **Gate run time.** Measured: of 1,056 s warm, 590 s is docs calls redone every run, 112 s the OpenSSL step, 80 s between jobs. No change made.
- **HeardRight.** Qualify stops on 38 ownership findings: 30 capabilities HeardRight still implements itself. Adrian: no exceptions, the code moves into RightKit. Not started.
