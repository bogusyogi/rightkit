# Protected ownership adapters

## Private local verifier

`private-local.mjs` is the fixed launcher. It accepts only fixed installer paths for Node & the adapter, audits every installed file/directory before spawning the fixed verifier, requires a clean checkout, imposes a 120-second verifier timeout, then validates a typed receipt against independently observed Git identity, commit/tree, protected registration, catalog, scanner, roots, content, & floors. The verifier scans committed Git objects in memory, so mutable working-tree bytes cannot substitute for source blobs. Failure, malformed output, timeout, path substitution, or ACL failure is closed.

Enforcement hosts launch fixed runtime & entry paths with `NODE_*`, `GIT_*`, `LD_*`, & `DYLD_*` injection variables removed & a protected native-tool PATH. Accept receipts only from that host's own verifier child. Adapter Git reads use the fixed native Git executable, disable filesystem-monitor commands, & have a 15-second limit. Broker owner owns admission integration.

| Platform | Protected root | Runtime | Launcher |
|---|---|---|---|
| Windows | `C:\Program Files\Orthic Labs\RightKit` | `runtime\node.exe` | `ownership\adapters\private-local.mjs` |
| macOS | `/Library/Application Support/Orthic Labs/RightKit` | `runtime/bin/node` | `ownership/adapters/private-local.mjs` |

The installer-owned tree includes Node, adapter files, imported `tools/rightkit/packages/release` source/catalog, & runtime dependencies. Its content inventory excludes exactly `ownership/trust.json`, `ownership/public-check.json`, `ownership/public-check-app.pem`, `ownership/checkpoints/`, & `ownership/installer/`; these policy/runtime-state/installer paths remain subject to full owner/ACL/link audits & their separate validators. Enrollment can update trust without changing scanner code inventory. Build a staging tree, then create its content-hash inventory with the packaged Node runtime:

```powershell
& 'D:\staging\RightKit\runtime\node.exe' 'D:\staging\RightKit\ownership\adapters\package-manifest.mjs' --root 'D:\staging\RightKit' --out 'D:\staging\RightKit\ownership\adapters\adapter-files.json'
```

An administrator verifies the inventory with `& 'D:\staging\RightKit\runtime\node.exe' 'D:\staging\RightKit\ownership\adapters\package-manifest.mjs' --verify --root 'D:\staging\RightKit'`, installs the complete tree at its fixed root, & applies protected ACLs. Windows permits native Users read masks plus only documented create rights on existing nonempty `C:\` & `C:\ProgramData`; every RightKit descendant must remain protected from user write, delete, reparse, ownership, & DACL changes. macOS install objects are root-owned, have no group/world write, reject links, & allow write ACLs only to exact `user:root`; `group:admin`, `group:wheel`, `group:system`, & substring principal matches do not grant write authority. Run the launcher non-elevated.

## Public protected check producer

`public-check-producer.mjs` runs only on an external protected worker. Config lives at the fixed `ownership/public-check.json` path & includes protected GitHub App/install IDs, the exact check name, service SID/UID, & an explicit agent SID/UID deny list. App key is at fixed `ownership/public-check-app.pem`; it is never stored in source or Actions secrets. The worker must use a dedicated noninteractive service account distinct from all coding-agent identities, with no administrator membership. Its service account can read the App key; only OS root/admin can administer it. Windows ACL audit rejects any other App-key reader & grants service identity read-only access. macOS requires service/root-only key ACLs & restrictive modes.

Before scanning, the producer verifies current process identity equals protected service identity, the source checkout is clean & exact-SHA, every checkout object is service-owned, and no non-service principal has write/delete authority. The service must use a private checkout directory unavailable to agent accounts & serialize work on that checkout. The fixed committed-source scanner reads Git blob bytes, protected trust/catalog/floors remain installer-owned, & a child scan process has a 120-second limit. Malformed output, scanner errors, timeout, source mismatch, trust rejection, or API authority mismatch produces a typed failure. On completion, the producer creates & reads back a Check Run bound to the exact commit & verifies GitHub-reported App ID, check name, status, conclusion, & SHA.

Provisioned JSON shape (values are administrator-owned):

```json
{
  "schema": 1,
  "appId": 123,
  "installationId": 456,
  "checkName": "RightKit protected ownership",
  "service": {
    "windowsSid": "S-1-5-21-...-1001",
    "agentWindowsSids": ["S-1-5-21-...-1002"],
    "windowsCheckoutRoot": "D:\\\\RightKit\\\\public-checkouts",
    "macosUid": 600,
    "agentMacosUids": [501],
    "macosCheckoutRoot": "/var/lib/rightkit/public-checkouts"
  }
}
```

For signed-key-log repositories, protected App config pins the historical root & initial checkpoint:

```json
{
  "rootFingerprint": "<64 lowercase hex characters>",
  "keyCheckpoint": { "sequence": 0, "head": "<64 lowercase hex characters>" },
  "keyAuthority": {
    "repositoryIdentity": "https://github.com/<owner>/<workspace-repository>",
    "checkoutPath": "<absolute protected workspace checkout>"
  }
}
```

When `keyAuthority` is present, both adapters read the signed log from that exact repository's committed `HEAD`; products need no per-repository copy. Keep this checkout & Git metadata protected and read-only to verifier service. Its canonical origin identity, exact commit & tree are independently checked and scanner evidence/receipts bind those values. Omitting `keyAuthority` retains legacy per-product committed-log mode.

Pre-create `ownership/checkpoints/` as an administrator-owned directory. Grant the isolated service identity write/create/delete rights only inside this exact subtree; deny agent identities. Keep installer code, App config, App key, & `ownership/` parent administrator-owned and service-read-only. The producer reads `.rightkit/ownership-keys.json` from exact authority Git `HEAD` when `keyAuthority` is configured (otherwise product `HEAD`), checks it against immutable config pin/floor & serialized `<rootFingerprint>.json` state, then flushes & atomically replaces state after valid fast-forward. Invalid, shorter, or forked logs leave config & state untouched. An existing per-root `.lock` fails closed; administrator recovery may remove only that exact lock after confirming no verifier holds it. Private receipts bind log digest, root fingerprint, sequence, head, & shared authority identity/commit/tree when configured; private trust supplies matching `keyAuthority`, `rootFingerprint`, & `keyCheckpoint`.

The external worker invokes fixed runtime & entry paths:

```text
<fixed-node> <fixed-root>/ownership/adapters/public-check-producer.mjs --repo-root <service-owned-checkout> --expected-commit <event-sha>
```

GitHub App needs check-write & repository read permissions. Branch protection must require `RightKit protected ownership` from that exact App ID. Do not give repo-controlled workflows the App key, protected config, trust, or a way to assert their own authority. The current checkout has no enrolled trust keys, App registration/credentials, service identity, external queue, or App-bound branch rule. This adapter package does not enroll or invent those authorities, provision the worker, or change workflow templates/branch protection; those are explicit protected-host rollout requirements.
