# rightkit-models 0.7.0 registry consumer (PK12)

Standalone consumer, outside the workspace, pinning `rightkit-models =0.7.0` exactly. It exercises
`hash::sha256_file`, the digest a model install checks an artifact against: the FIPS 180-4 vector for
"abc", a different digest after the bytes change, and a refusal for a missing file.

It also proves the one store of 0.7.0: a small two-file model installed through `Store` (adopted
from a flat folder, every file pinned) lands in the suite layout, `<root>/<id>/<files>` plus the
suite `manifest.json` (version = revision key), with no `state.json` or `revisions/`; the suite
reader (`suite::manifest::verify_dir`) verifies it at full depth; `Store::resolve` serves the entry;
`Store::rollback` has nothing to roll back to. An entry the pre-0.7 revisioned store wrote
(`state.json` + `revisions/<key>/`) is read, then migrated onto the suite entry by the first
`resolve`. Uninstall removes the entry.

State: a per-process directory under the OS temp directory, removed at the end. No network, no
process spawn, no real user data. Run only by the CI consumer lane.

Not covered: downloads, archive extraction, suite entitlements, and probes.
