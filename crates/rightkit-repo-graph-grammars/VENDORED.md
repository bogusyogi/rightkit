# Vendored tree-sitter grammars

Three grammars stay vendored (`ql`, `systemrdl`, `vue`); each parser.c is under 2 MiB. The other five
Blueprint grammars come from crates.io, pinned exactly in `membrane-blueprint/Cargo.toml`:
`tree-sitter-elisp =1.7.2`, `tree-sitter-embedded-template =0.25.0`, `tree-sitter-solidity =1.2.13`,
`tree-sitter-tlaplus =1.5.0`, `arborium-rescript =2.18.2` (the same upstream grammars; their generated parser.c
files live in the registry, not in this repository).

Why these three stay:
- `ql`: the crate `tree-sitter-ql 0.23.1` is a rewritten grammar (only 21 node kinds in common; `class`,
  `classname`, `modulename` are gone, `dataclass`, `classlessPredicate`, `moduleName` replace them), so it would change
  Blueprint output and `fallback_declaration_name`'s ql arms.
- `vue`: the crate `tree-sitter-vue 0.0.3` is a different upstream (xiaoxin-sky), needs tree-sitter ~0.20 and has no
  `LanguageFn`.
- `systemrdl`: no crate exists.

Each remaining `grammars/<lang>/` directory is a vendored checkout of the upstream grammar
repository at the exact commit/tag that the legacy JS Blueprint's tree-sitter-wasms
bundle (`tree-sitter-wasms@0.1.13`, resolved 2026-09-10) pinned. Where the upstream
repo shipped a generated `src/parser.c` at an ABI tree-sitter supports
(ABI 13-15), it is used unmodified. Where `src/parser.c` was missing it was regenerated locally with
`tree-sitter-cli 0.26.13` via `tree-sitter generate`.

| lang | repo | ref | commit | parser.c source | notes |
|---|---|---|---|---|---|
| ql | https://github.com/samlanning/tree-sitter-ql.git | v1.0.0 (npm `tree-sitter-ql@^1.0.0`) | tag v1.0.0 | regenerated (tree-sitter-cli 0.26.13, ABI 14) | no LICENSE file upstream; package.json declares MIT |
| systemrdl | https://github.com/SystemRDL/tree-sitter-systemrdl.git | v0.7.0 (npm `tree-sitter-systemrdl@^0.7.0`) | commit `fb7e4134e478962fb70097b42a767364f4928a5e` (repo has no git tags; commit resolved via npm registry `gitHead` for the 0.7.0 publish) | regenerated (tree-sitter-cli 0.26.13, ABI 14) | |
| vue | https://github.com/tree-sitter-grammars/tree-sitter-vue.git | pinned by tree-sitter-wasms | commit `7e48557b903a9db9c38cea3b7839ef7e1f36c693` | vendored as-is (ABI 14) | |

Each grammar directory carries its own `LICENSE` copy from upstream where the
upstream repo provided one (`ql` did not; its upstream `package.json` declared
`"license": "MIT"`, recorded here for provenance). The upstream `package.json` of
every grammar was deleted: nothing reads it (`build.rs` compiles only `src/parser.c`
and `src/scanner.c`), and its npm dependencies (`nan`, `node-addon-api`,
`tree-sitter-cli`, ...) are not dependencies of this crate. All declared MIT.

Nested `.git` metadata is removed from every vendored grammar; all grammar
sources are ordinary files owned by this repository. `build.rs` compiles only
`src/parser.c` plus any `src/scanner.c`.

`elm` is **not** vendored here — it is native via the
`tree-sitter-elm = "5.9.4"` crates.io crate directly in `membrane-blueprint`'s
`Cargo.toml`. The 27 direct parser crates provide 28 catalog languages because
`tree-sitter-typescript` supplies both TypeScript and TSX; five more come from the crates above and three use this vendored crate.
