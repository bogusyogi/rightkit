# @rightkit/ci npm consumer (draft, npm.@rightkit/ci)

Packed-archive consumer for `@rightkit/ci` (workspace version). The unit is installed only from the sealed `file:`
archive. The reviewed closure has no registry dependencies: `packages/ci/package.json` declares none, and the
package imports only Node built-ins. No `@rightkit/*` package is in the closure.

Surface: `@rightkit/ci` declares no `exports` and no `main`. Its public surface is the `right-git` bin at
`cli/right-git.mjs` and the module files its `files` list ships (`*.mjs`, `policy`, `templates`, `*.json`). The
consumer never imports the package root; it executes the installed bin file with Node and imports the installed
`manifest.mjs`, `sync.mjs`, `install.mjs` and `policy/visibility-gate.mjs` by file URL.

Scenarios (7): installed bin manifest bound to the archive (name/version, exact `bin` mapping, no `exports`, no
dependencies, Node shebang, templates and schema shipped); installed CLI `--version` and usage smoke (exact version,
`--help` lists every command, no arguments exits 1, an unknown command and a command without its repository exit 2);
manifest validation accepts a minimal rust-hybrid manifest and refuses a bad lane, schema version, compile mode,
missing gate and an expression in the matrix; `renderLanes` renders a SHA-pinned, deterministic `ci.yml` for a
rust-hybrid manifest and the node profile lanes; `gate.shards` renders `matrix.shard`, `--group`, the `quick` job and
the `ci-summary` job with per-leg state, and its validation refusals; a manifest without `gate.shards` renders no
shard tokens; private, internal and unconfirmed repositories are refused by `installCommand` while a public one
plans a create, and nothing is written.

No network and no user data. Every process and network call in the visibility scenario is an injected fake: `gh`,
`git` and `fetch` are never started. The repository the fixtures render for is a path that is never created. The
CLI is invoked only with `--version`, `--help`, no arguments, an unknown command and `install`/`sync` without a
repository. Writes nothing outside the scratch install.
