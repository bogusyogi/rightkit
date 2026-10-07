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
