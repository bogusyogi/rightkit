# @rightkit/legal npm consumer (draft, npm.@rightkit/legal)

Packed-archive consumer for `@rightkit/legal` 0.6.2 (workspace version). The unit is installed only from the sealed
`file:` archive; the reviewed closure has no registry dependencies. `@rightkit/release` is an optional peer and is
declared only, never installed.

Surface (exact `exports` keys): `.`, `./browser`, `./tooling`.

Scenarios (6): default policy render digest binding; manifest validation and declaration without a clock; receipt
currency until a material change; material digest tracks agreements only, with Web Crypto parity; unsafe manifests
refused; tooling subpath isolated from the root entry.

No network, no user data. All fixture URLs use the reserved `.invalid` TLD and are never fetched. Writes nothing.
