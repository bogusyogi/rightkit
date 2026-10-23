# @rightkit/issues-tracker npm consumer (draft, npm.@rightkit/issues-tracker)

Packed-archive consumer for `@rightkit/issues-tracker` 0.1.0 (workspace version). The unit is installed only from
the sealed `file:` archive. The reviewed closure has no registry dependencies (`packages/issues-tracker/package.json`
declares none).

Surface: the package declares no `exports` and no `main`. Its public surface is the `right-issues` bin at
`cli/right-issues.mjs` and the shipped module file `tracker.mjs` (the `files` list is `cli` and `tracker.mjs`).
The consumer never imports the package root; it executes the installed bin file with Node and imports the installed
`tracker.mjs` by its file URL.

Scenarios (3): installed bin manifest bound to the archive (name/version, exact `bin` mapping, no `exports`, Node
shebang, no dependencies); installed CLI `--version` smoke (exit 0, stdout exactly the expected version); tracker
module renders a self-contained, escaped page from a fixture (status from labels, repository name escaped in the
title, one closing script tag, no external resources, deterministic across two renders).

No network, no user data, writes nothing. Fixture issue URLs are literal github.com paths that are never fetched.
