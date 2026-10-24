# Package audit, 2026-10-24

Read-only audit of every RightKit npm package and crate, and of the nine apps' pins, after PR #945. Four lane files:
[npm.md](npm.md), [crates-a-l.md](crates-a-l.md), [crates-m-z.md](crates-m-z.md), [versions-and-pins.md](versions-and-pins.md).
The crate and graph lanes compared against live crates.io and npm; nothing was built. Claims marked UNVERIFIED there are not
established.

## What can publish, in order

1. `rightkit-tauri-shell` 0.6.0: every crate in its closure is published; recipe present and hash-matched. Dispatched from
   `main` (`a3ae84cc`) as publish run 40046899968. Its gate rows do not cover the new default feature `wv2-0_39`.
2. `@rightkit/release` 0.6.4: no unpublished `@rightkit/*` dependency, but it waits on its canary (Mac and Dell legs, P10),
   and it calls `rightkit-qa journeys`, which exists only in unpublished source.
3. `rightkit-child-process` **0.5.6** (0.5.5 is live from older source; ~330 lines landed under the same number in 7d64724).
4. `rightkit-qa` **0.9.0** (0.8.0 is live; wave 2, ~35k lines with a breaking change, sits under 0.8.0). It uses the new
   child-process APIs, so it follows 3. Its CHANGELOG still says wave 2 is "not yet compiled or tested" while 7d64724 reports
   passing runs: settle which is true before publishing.
5. Then repin apps, `@rightkit/release` first (it carries the newer version feed); each repin needs its
   `minimumReleaseAgeExclude` entry in the app's `pnpm-workspace.yaml`.

## Same version, different source (bump before any publish)

`rightkit-qa` 0.8.0, `rightkit-child-process` 0.5.5, `@rightkit/qa` 0.6.1 (errored/missing statuses, evidence throw),
`@rightkit/outdated` 0.2.0 (calls `rustright`), `rightkit-tts` 0.2.3 and `rightkit-tts-qwen-sys` 0.1.2 (dependency changes).
Everything else that differs is the rename in docs and comments only; `rightkit-ownership`'s embedded catalog text changed
(digest effect UNVERIFIED).

## Hygiene, no release impact

- 28 crate CHANGELOGs mark a published version "unpublished"/"unreleased"; several have duplicate headings; `@rightkit/hooks`
  has no CHANGELOG; `app-update` 0.4.2 and `tauri-shell` 0.1.3 have no heading.
- 38 units have no reviewed consumer recipe (listed in `ci/recipe-coverage.json`); any bump of them is refused until one lands.
- `ci/consumer-recipes.json:667` records `rightkit-db` `=0.5.1` while its recipe and crate are 0.5.2 (hashes still match).
- `rightkit-http` lists a missing `API.md` in `include`; `ax`, `ci`, `hooks` declare MIT OR Apache-2.0 but ship no licence
  files; `tauri` pins `@rightkit/logs` with a range.
- Apps: no forbidden path/git/link specs. Every app is behind on some pins (mailright most); inconsistent duplicates in
  genright (`rightkit-transcript`), mailright (`rightkit-credentials`) and CutRight (`rightkit-http`).
