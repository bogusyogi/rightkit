# rightkit-models 0.5.0 registry consumer (PK12)

Standalone consumer, outside the workspace, pinning `rightkit-models =0.5.0` exactly. It exercises
`hash::sha256_file`, the digest a model install checks an artifact against: the FIPS 180-4 vector for
"abc", a different digest after the bytes change, and a refusal for a missing file.

State: a per-process directory under the OS temp directory, removed at the end. No network, no
process spawn, no real user data. Run only by the CI consumer lane.

Not covered: downloads, installs, the store layout, suite entitlements, and probes.
