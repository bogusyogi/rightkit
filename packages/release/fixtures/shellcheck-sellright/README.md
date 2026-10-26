# SellRight shell scripts: class A ShellCheck regression fixtures

These files are SellRight's own scripts, byte for byte, at the commits where its CI failed on ShellCheck and where
it repaired them. `deterministic-scans.test.mjs` and `rightkit-gate.test.mjs` run them through the real class A
`shellcheck` scan (`rightkit-gate check`) and hold the verdicts below. They are stored as `*.sh.fixture` so that
RightKit's own class A scan, which checks every tracked `.sh`/`.bash` file, does not judge the red ones; the tests
copy each to its original path in a scratch repository first.

| Fixture | Source path | Commit | Git blob | Verdict under ShellCheck 0.11.0 |
| --- | --- | --- | --- | --- |
| `sellright-a09236e-parent.sh.fixture` | `deploy/sellright.sh` | `75ee586910e6ea3911b48cfec424dbd332e506f8` (`a09236e^`) | `e856d8c974fa35a44c5173baa1ffa3df2c5b576c` | red: SC2154, SC2030, SC2031 |
| `sellright-a09236e.sh.fixture` | `deploy/sellright.sh` | `a09236e033d509f03d0eb5ae8d9bcc0edca4f309` | `234308df35d076032a7cf2df04f986e153736d50` | green (the repair) |
| `sellright-6c3faed-parent.sh.fixture` | `deploy/sellright.sh` | `79484e4ad30ea8183437d5855acfb81fbcd4cc09` (`6c3faed^`) | `07af9d2a39fa54241aca9e0c4e4684f1a108e074` | red: SC2016 |
| `sellright-6c3faed.sh.fixture` | `deploy/sellright.sh` | `6c3faeda0b66f191ecc3955d116f7ace18262d59` | `9fa44bf81ee74de9edb34869b3cd2c6450052d27` | green (the repair) |
| `test-installer-image-binding-059fe34-parent.sh.fixture` | `deploy/tests/test-installer-image-binding.sh` | `e5d788195bd382c52773422a90f0070119e161a6` (`059fe34^`) | `3811ed592b1de00dea9a19dc001ea52a1d6ee362` | red: SC2329, SC2016 |
| `test-installer-image-binding-059fe34.sh.fixture` | `deploy/tests/test-installer-image-binding.sh` | `059fe347d69d1d1e73e20a06e99cec76dd01bc01` | `a5db74a35654e22673d6515a57fdd0af04087197` | green: the positive control (`# shellcheck disable=` comments that say why) |

Source repository: <https://github.com/Orthic-Labs/sellright>. The tests check each file's git blob id against the
table, so an edit to a fixture fails instead of silently testing something SellRight never committed.

ShellCheck 0.10.0 gets these wrong: it fails both repairs on SC2015 and the positive control on SC2317 (0.11.0 moved
"function never invoked" to SC2329, which is the code SellRight disables). That is why class A accepts only the
version in `PINNED_SCAN_TOOLS` (`packages/release/deterministic-scans.mjs`).

## License and provenance

- `sellright-a09236e*` and `test-installer-image-binding-059fe34*`: at those commits SellRight is licensed under the
  Apache License, Version 2.0 (the repository's `LICENSE`). Its `NOTICE` reads:

  > SellRight
  > Copyright 2026 Adrian D'Souza and Rotten Hand LLC d/b/a Orthic Labs

- `sellright-6c3faed*`: at that commit SellRight was licensed under the Business Source License 1.1 (Licensor:
  Adrian D'Souza and Orthic Labs LLC; (c) 2026). BUSL-1.1 permits copying, redistribution and non-production use;
  these bytes are used only as test input. The same licensor later relicensed SellRight under Apache-2.0
  (`a09236e` and `059fe34` above carry that licence).

The files are unmodified copies: no change of content, line endings or mode beyond dropping the executable bit.
They are test data only, never executed, and not part of the published `@rightkit/release` package (no `files`
pattern in its `package.json` matches `*.sh.fixture`).
