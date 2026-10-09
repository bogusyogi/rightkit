# RightKit ownership reporting & lookup

`packages/release/rightkit-ownership.json` is the canonical capability catalog.
`rightkit-versions.json` supplies registry pins. A value in `npm`, `cargo`, or
`swift` is reported as registry-pinned by the manifest; staged values are
reported as staged-unverified. This catalog alone does not independently
verify a live registry. An absent entry means availability is not recorded.
Lookup never infers API shape, supported
platforms, minimum versions, or migration instructions unless the catalog
contains that field.

Use `rightkit owns <capability>` for a single entry. Use
`rightkit owns --compact` for the compact session-start index; host startup
integration must inject this output as context. This lookup reports ownership
and availability. A declared dependency alone does not establish integration.

The local cross-app report derives core app roots from
`packages/release/standalone-clone-verify.mjs` and adds configured migration
roots for CutRight, GenRight, RightSites, & VoiceRight. CodeRight is scanned at
repository scope to include engine source; Membrane is a separate scan scope at
`coderight/context/apps/membrane-hub` inside that same Git repository. Report
rows keep scan scope separate from repository identity, including CodeRight's
nested `apps/coderight-tauri` manifest coverage:

```powershell
right-release ownership-report --workspace D:\Claude `
  --markdown tools/rightkit/docs/OWNERSHIP-REPORT.md `
  --json-out tools/rightkit/docs/OWNERSHIP-REPORT.json `
  --timeout-ms 60000
```

Each app scan runs in an isolated worker with a bounded timeout; a stalled
worker is recorded as `unscanned` so one repository cannot hold the full report.

Each capability row uses one of these states:

| State | Meaning |
|---|---|
| `not-detected` | No ownership fingerprint or dependency declaration was detected in scanned inputs. |
| `declared-dependency` | App declares a RightKit dependency; this does not prove the capability is integrated or used. |
| `verified-integration` | Integration evidence explicitly returned by the scanner. |
| `excepted` | Scanner validated a signed exception against protected trust and current findings. |
| `blocked` | Scanner found an unexcepted ownership finding or a blocking exception problem. |
| `unscanned` | Coverage is incomplete for this app or capability. |

Reports carry host coverage, catalog version & digest, repository identity,
commit, scan time & age, coverage gaps, and content digests. `contentDigestSource`
identifies scanner input digests or, until those are available, finding-only
occurrence hashes. `blocks` and
`exceptionRequests` are deduplicated by content hash. Exception request feed
entries are unverified requests; they do not indicate approval. Only scanner
validated signed records may produce `excepted` status.

The local scan describes the checkout on the host that produced it. It does
not establish coverage on another host; run the report on each required host
and retain both outputs to demonstrate cross-host coverage.

## Deployment requirements

Source checks are implemented; installed enforcement is not active. Neither
host has enrolled approval keys or a protected ownership registry. Native
approval helpers require user-initiated enrollment and prompt validation;
Windows also needs its workspace-provisioned .NET SDK before native compilation.
Private repository enforcement requires an OS-protected adapter using
`runProtectedLocalOwnershipCheck`, plus exact repository roots in external trust.
Write feedback and session context must load from an installer-owned stable
RightKit root before host hook registration; development paths are not installed
enforcement.

Generated public CI templates call the protected ownership command
unconditionally. Hosted runners currently have no protected trust delivery, and
the verifier supports fixed Windows/macOS paths only. These checks fail closed
until protected CI deployment exists. A repository-supplied trust file, workflow
override, or unprotected runner copy cannot supply authority. Public required
ownership checks need an external protected verifier bound to the exact commit,
catalog, source bytes and registered roots, with protected scanner/catalog floors.

Broker admission remains the design in `../OWNERSHIP-BROKER-DESIGN.md`.
The Claude session “RightKit build infrastructure recovery” owns both-host
implementation/cutover coordination and must let running jobs finish first.
No broker bootstrap, activation, cancellation or forced restart belongs to this
source change. Crate and `@rightkit/release` publication requires two-host gate
evidence at the exact publication commit.

## Protected policy bundle

Protected trust pins more than the catalog digest. `trust-sync` writes a `policyBundle` into protected trust:
one digest over the ownership catalog, the repository registry file, the dependency allowlist, the scanner
version, and the SHA-256 of every scanner/verifier source file in the import closure of the enforcement entry
points (`ownership-policy-bundle.mjs`: scan worker, evidence, verdict, admission, staged/hook gates, trust
updater, plus pinned third-party parsers such as `smol-toml`). CRLF is normalized, so a checkout's line-ending
policy does not change the digest.

`ownership-evidence.mjs` computes the running bundle and compares it with the pinned digest through the floor
check. A mismatch sets `floorOk: false`, adds an `ownership policy bundle ... differs` problem, and refuses
release/admission and sealed-evidence verification, so editing scanner code or the allowlist while keeping the
catalog no longer passes. Trust written before the bundle existed has no `policyBundle` and is treated as
unpinned until the next `trust-sync`. While approval enrollment is pending the mismatch is a diagnostic only,
as for every other floor problem. Any change to a pinned file (a normal RightKit release that touches the
scanner) therefore needs a `trust-sync` before protected release gates pass again; that is intentional.

The `trust-sync` approval summary shows a semantic diff: bundle digest change, scanner version, scanner files
changed/added/removed, catalog and registry changes, and allowlist entries added/removed per repository.

## Default-deny dependency allowlist

`packages/release/ownership-dependency-allowlist.json` lists approved direct dependencies as
`<ecosystem>:<name>[#git|#path]` (registry is the default kind), per canonical repository identity, plus a small
`global` list. Seeded from each registered repository's current direct dependencies (all manifest sections of
`Cargo.toml`, `package.json`, `Package.swift`) with
`node packages/release/ownership-dependency-allowlist-seed.mjs --workspace <dir> [--write]`; the seed only adds
approvals and never records block-tier catalog dependencies or RightKit packages.

In a guarded repository a new direct dependency is allowed only if it is a RightKit package, a first-party local
`path`/`workspace` dependency, in that repository's baseline, or in the global list. Known block-tier capability
dependencies override any allowlisting.

- **Write hook** (`ownership-write-hook.mjs`): denies a new dependency (manifest edit or `cargo/npm/pnpm add`)
  that is not allowlisted: "propose it in ownership-dependency-allowlist.json; Adrian activates it via
  trust-sync". It reads the activated copy from protected trust (`dependencyAllowlist`, guarded repositories from
  trust `repositories`); when trust has no allowlist yet it uses the shipped byte copy in advisory mode. The
  shipped files travel with the other consumer copies (`rightkit-sync` and `install-policy-assets`).
- **Release/committed scan** (`ownership.mjs`): reports `dependency-not-allowlisted` as a blocking finding when
  the protected copy governs the repository; advisory while approval enrollment is pending, and advisory from the
  shipped proposal before `trust-sync`.

Agents can propose edits to the file; it takes effect only when Adrian runs `trust-sync`, which copies it into
protected trust (an invalid proposal leaves trust untouched).

## Cross-host trust comparison

`right-release ownership trust-compare [--json]` prints this host's trust digest (canonical JSON of protected
trust excluding only the per-host key-authority checkout path) and its component digests (policy bundle,
allowlist, registry, floors, key checkpoint, root). There is no signature on the trust file and no automation.
Treat the Mac's digest as the reference: agents on the Mac have no root, so its trust cannot be rewritten by an
agent. After both hosts have synced the same release, a Dell digest that differs from the Mac's means the Dell
trust file was tampered with (or is stale: re-run `trust-sync` there and compare again). Compare the component
lines to see which part differs.
