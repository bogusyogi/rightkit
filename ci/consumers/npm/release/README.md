# @rightkit/release npm consumer (draft, npm.@rightkit/release)

Packed-archive consumer for `@rightkit/release` 0.5.1 (workspace version). The unit is installed only from the
sealed `file:` archive. The reviewed closure pins its sole registry dependency, `smol-toml` 1.9.1, recorded in
`packages/release/package.json`. No `@rightkit/*` package is in the closure.

Surface: `@rightkit/release` declares no `exports` and no `main` in `packages/release/package.json`. Its only
public surface is the `right-release` bin at `cli/right-release.mjs` and, since 0.5.0, the `rightkit-gate` bin at
`bin/rightkit-gate.mjs`. The consumer therefore never imports the package root; it executes the installed
`right-release` bin file directly with Node and checks both bins' manifest mapping and shebangs.

Scenarios (3): installed bin manifest bound to the archive (name/version, exact `bin` mapping, no `exports`,
Node shebang); installed CLI `--version` smoke (exit 0, stdout exactly the expected version); `smol-toml`
closure installed and loads (dependency resolved from the installed release package, pinned at 1.9.1, parses a
TOML fixture).

No network, no user data, no release execution. Only `--version` is invoked; no build, publish, signing or
upload command runs. Writes nothing outside the scratch install.
