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
  --markdown rightkit/docs/OWNERSHIP-REPORT.md `
  --json-out rightkit/docs/OWNERSHIP-REPORT.json `
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
| `excepted` | A plain entry in the repo exceptions file matches the finding (capability and package). |
| `blocked` | Scanner found an unexcepted ownership finding or a malformed exception entry. |
| `unscanned` | Coverage is incomplete for this app or capability. |

Reports carry host coverage, catalog version & digest, repository identity,
commit, scan time & age, coverage gaps, and content digests. `contentDigestSource`
identifies scanner input digests or, until those are available, finding-only
occurrence hashes. `blocks` and
`exceptionRequests` are deduplicated by content hash; `exceptionRequests` now
lists stale exception entries (entries that match no current finding).

## Exceptions

Exceptions are plain entries in `.rightkit/ownership-exceptions.json`, reviewed
through git like any other change (Orthic-Labs/rightkit#262; signed exceptions
are retired under the proportional-trust rule, as trust-sync was). No signature,
Touch ID prompt, native helper, protected trust or enrolled key is involved, and
agents may edit the file directly.

```json
{ "exceptions": [
  { "capability": "browser-automation", "package": "rightkit-browser",
    "reason": "why the app keeps its own copy", "owner": "app or person",
    "date": "2026-10-17" }
] }
```

- `capability` and `package` (`crate` is accepted as an alias) must equal the
  catalog capability and owner of the finding. Every finding of that capability
  and package is `excepted`.
- Optional `fingerprint` (a catalog fingerprint id) and `path` (an exact file or
  directory prefix) narrow an entry to part of a capability.
- `reason`, `owner` and `date` (`YYYY-MM-DD`) are required. A missing field or a
  legacy signed record (`schema`/`payload`/`signature`) is a problem, so the scan
  fails and names the entry.
- An entry that matches no current finding is **stale**: it is reported as a
  warning (`exception #N (...): stale, matches no current finding`), never a
  block. Remove it.
- Pending approval enrollment is a reported warning everywhere, never a failure (`checkApp` / `checkCommitted`,
  protected evidence, the verdict, broker admission and the Rust `check_app`, `create_evidence` and `ownership_verdict`):
  enrollment existed only for signed approvals. `enrollmentPending` is still reported, and an admitted verdict carries a
  `warnings` entry. Every real gate stays: an unexcepted block-tier finding, a malformed exception entry, incomplete
  coverage, a scanner or catalog floor downgrade, an unregistered repository when a protected registry exists, and the
  `trust.json` write guard. With no trust registry on the host at all, the registry and floor checks are skipped with a warning.
- `right-release ownership-approval` (draft, sign, verify, import) is a retired
  no-op that prints this pointer. `verifyException` and the native signing path
  are gone; the approval keys and key log remain only for the unrelated trust
  machinery below.

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

Protected trust can pin more than the catalog digest: a `policyBundle` (written by the now-retired `trust-sync`):
one digest over the ownership catalog, the repository registry file, the dependency allowlist, the scanner
version, and the SHA-256 of every scanner/verifier source file in the import closure of the enforcement entry
points (`ownership-policy-bundle.mjs`: scan worker, evidence, verdict, admission, staged/hook gates, trust
updater, plus pinned third-party parsers such as `smol-toml`). CRLF is normalized, so a checkout's line-ending
policy does not change the digest.

`ownership-evidence.mjs` still computes the running bundle and compares it with a pinned digest, but a difference is
a **warning**, never a block. `trust-sync`, the human-approved way to re-pin the bundle, is retired (Adrian,
`decisions.md`): no release, ownership, admission, sealed-evidence or QA check refuses on a missing or stale pinned
bundle. The drift is reported as `ownership policy bundle ... differs from the bundle pinned in protected trust` in
the scan output (`WARNING` line and the JSON `warnings` field) and by `right-release ownership trust-compare`, which
adds a semantic diff (scanner version, scanner files added/removed/changed, catalog, registry, allowlist and QA floor
digests). Trust with no `policyBundle` has nothing to compare. `right-release ownership trust-sync` is a no-op that
prints why and exits 0. A QA receipt still records the bundle digest it was written under; that binds the receipt to
the code that produced it and does not depend on trust.

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
  that is not allowlisted: "propose it in ownership-dependency-allowlist.json (advisory until protected trust
  carries it; trust-sync is retired)". It reads the copy from protected trust (`dependencyAllowlist`, guarded repositories from
  trust `repositories`); when trust has no allowlist yet it uses the shipped byte copy in advisory mode. The
  shipped files travel with the other consumer copies (`rightkit-sync` and `install-policy-assets`).
- **Release/committed scan** (`ownership.mjs`): reports `dependency-not-allowlisted` as a blocking finding when
  the protected copy governs the repository; advisory while approval enrollment is pending, and advisory from the
  shipped proposal when no trust copy applies.

Agents can propose edits to the file. With `trust-sync` retired, nothing copies a proposal into protected trust, so
a proposal stays advisory until a protected trust copy carries the allowlist.

## Cross-host trust comparison

`right-release ownership trust-compare [--json]` prints this host's trust digest (canonical JSON of protected
trust excluding only the per-host key-authority checkout path) and its component digests (policy bundle,
allowlist, registry, floors, key checkpoint, root). There is no signature on the trust file and no automation.
Treat the Mac's digest as the reference: agents on the Mac have no root, so its trust cannot be rewritten by an
agent. After both hosts have synced the same release, a Dell digest that differs from the Mac's means the Dell
trust file was tampered with or is stale (a drifted policy bundle is reported as a warning, not a block). Compare the component
lines to see which part differs.
