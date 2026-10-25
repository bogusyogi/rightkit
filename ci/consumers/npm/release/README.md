# @rightkit/release npm consumer (draft, npm.@rightkit/release)

Packed-archive consumer for `@rightkit/release` (workspace version). The unit is installed only from the
sealed `file:` archive. The reviewed closure pins `smol-toml` 1.9.1 and the updater `@rightkit/outdated` 0.2.0 with
the `@rightkit/ci` 0.2.49 that the published updater resolves, all recorded in `ci/consumer-recipes.json`; the
consumer manifest lists exactly those pins.

Surface: `@rightkit/release` declares an explicit `exports` map in `packages/release/package.json`: one `"./<file>"`
key per supported subpath (the two bin files, `./package.json`, the README's SDK modules and the modules product
repositories import today; inventory in `docs/RELEASE-EXPORTS.md`), no `"."` index and no wildcard. The reviewed
surface is that key list. The consumer never imports the package root: it executes the installed `right-release`
bin file directly with Node, checks both bins' manifest mapping and shebangs, and checks the map through Node's
own resolver.

Scenarios (5): installed bin manifest bound to the archive (name/version, exact `bin` mapping, Node shebang);
installed exports map explicit and enforced (keys are `./<file>` mapped to themselves, each file is in the
archive, bins and `package.json` are listed, listed subpaths resolve inside the installed package, the package
root and internal modules fail with `ERR_PACKAGE_PATH_NOT_EXPORTED`, an exported module imports); installed CLI
`--version` smoke (exit 0, stdout exactly the expected version); installed `right-release update --version`
(runs the `right-update` bin of the pinned `@rightkit/outdated` found beside the installed release package, prints
the pinned version; pnpm links no `right-update` at an app root, which is why the command exists); `smol-toml`
closure installed and loads (dependency resolved from the installed release package, pinned at 1.9.1, parses a
TOML fixture).

No network, no user data, no release execution. Only `--version` is invoked; no build, publish, signing or upload command
runs. Writes nothing outside the scratch install.
