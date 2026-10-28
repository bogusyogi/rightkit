# The gate contract

Adrian's requirements for the single gate, the end state, what success looks like, what must not move, the failures not to
repeat, and the corrective plan. One page, owned by Adrian. `scripts/ci/gate-contract.test.mjs` holds the code to sections 1
to 4; `ci/gate-contract.json` is the end-state matrix in data. A change that cannot be expressed as a cell of that matrix going
green, or a cell Adrian added, is not made. Written 2026-10-28.

## 1. Requirements (verbatim)

> rightkit releases from mac, dell or linux. on dell and mac, via broker. on linux no broker. but in all cases single point of
> entry with class a first, local compile, qualification on CI and auto publish from git.
>
> private repo apps built on mac, dell or linux. on dell and mac, via broker. on linux no broker. but in all cases single point
> of entry with class a first, local compile, qualification held for dell or mac.
>
> public repo apps built on mac, dell or linux. on dell and mac, via broker. on linux no broker. but in all cases single point of
> entry with class A first, local compile, qualification on CI. signed installers available.

Standing definitions, Adrian's words:

- Class A is everything mechanical, everything deterministic: format, lint, typecheck, syntax, lockfiles, licences, secrets,
  advisories, workflow lint, generated-file drift, the app rules, and deterministic tests. It runs first, and nothing compiles while
  it is red. No green-then-red.
- Local compile is on the primary checkout with a warm cache. No clone, worktree, branch or stash for a build. Never cold.
- One build per app per host. Different apps may build together within the host's resources. A newer build of the same app
  supersedes the older one.
- A dev build is the production build plus QA. Same signing, same identity, one installed copy.
- Gates stop agents forgetting or skipping steps. Nothing is built to defend against forgery by an agent. A person's own terminal
  is out of scope.
- Publication is automatic from git once qualification is green. Upload to R2 or a store is a separate step from build.

## 2. End state

One entry: `pnpm gate:<stage>` in every repository (the `gate:*` scripts of `packages/release/gate-scripts.mjs`, written by
`rightkit-gate adopt`), which runs `rightkit-gate`. The gate chooses the executor by host policy, never by what happens to be on
PATH: the RustRight broker on macOS and Windows (the Mac and the Dell), direct execution on every other host (Linux boxes, cloud
sessions, CI runners). On a managed host an app's own copy of the gate hands the run to the host's pinned gate, so the entry is the
same wherever it was typed.

Stages, in order, each only after the one before: `check` (class A), `compile`, `package`, `qualify`, `release`.

| Origin | RightKit unit (`@rightkit/*`, `rightkit-*`) | Private app | Public app |
|---|---|---|---|
| Mac (broker) | class A, compile and package through the broker, qualify locally, dispatch to GitHub, GitHub qualifies the exact bytes and publishes to npm or crates.io | class A, compile, package, installed QA on this host; stage to the publisher; R2 upload by the publisher | class A, compile, package; qualification and signed installers in CI; GitHub Releases by the publisher |
| Dell (broker) | same as the Mac, Windows toolchain | same, Windows installer | same, Windows installer |
| Linux (no broker) | same stages, direct execution; dispatch to GitHub; GitHub qualifies and publishes | class A, compile, package; qualification is held for the Mac or the Dell (no installed-QA lane on Linux) | class A, compile, package; qualification and signed installers in CI |

Every cell is a row of `ci/gate-contract.json` with its executor, the stages it reaches, and where its qualification runs.

## 3. What success looks like

A cell is green on three layers, and the release is done only when every cell of its scope is green on all three.

1. **Contract binding.** The rule ids, scan prefixes, stage names, entry scripts and host policy in the code equal the ones in
   this document and the matrix. Fails on any drift, in seconds, on any host.
2. **Matrix.** For every cell, the gate is driven with injected host facts (platform, PATH, environment) and asserted to pick the
   cell's executor and to reach or refuse exactly the cell's stages, with the reason. Pure, seconds, any host.
3. **Proven.** One real run per origin, started from the repository's own entry (`pnpm gate:<stage>`), that writes a receipt naming
   the commit, the host, the executor, each stage reached and its artifact digests. A reading of the code is not proof. A synthetic
   broker request is not proof. A receipt from a clone, a worktree or a copy is not proof. The receipt is stored against the commit
   and artifact hashes outside the source it describes (a receipt committed into its own tree changes the revision it names).

Done means: 0.6.9 published to npm from a real run on a Linux host, the same command green on the Mac and the Dell with their
brokers, one private app and one public app through their nine cells, every receipt on file.

## 4. End-to-end tests

Each is one command from the entry, run by a person or a chat on the real host, and judged by its receipt.

| Cell | Command | Expected receipt |
|---|---|---|
| RightKit unit, any origin | `pnpm gate:release -- --unit npm:@rightkit/release` from the RightKit checkout at a clean `main` | stages check, compile, package, qualify, release all `ok`; executor `broker` on the Mac and the Dell, `direct` on Linux; the GitHub qualification run id; the registry serving the sealed sha256 |
| RightKit unit, red tree | the same on a tree with one formatting error | stops at `check`, nothing compiled, the finding named |
| Private app, Mac or Dell | `pnpm gate:qualify --platform mac` (or `win`) in the app's checkout | check, compile, package, qualify `ok`; sealed manifest digest; executor `broker` |
| Private app, Linux | `pnpm gate:package --platform linux` | check, compile, package `ok`, executor `direct`; qualify refused with "held for the Mac or the Dell" |
| Public app, any origin | `pnpm gate:package --platform <p>` then `pnpm gate:stage --platform <p> --tier patch` | package `ok` locally; the publisher run id; signed installer digests from CI |
| Managed host, wrong gate copy | `pnpm exec rightkit-gate --to compile` from an app pinned to an older `@rightkit/release` | the app's copy hands the run to the host's pinned gate; the broker receipt names the pinned gate as `callingGate` |
| Agent, any origin | an agent shell running `cargo build`, `tauri build` or `vite build` directly | denied by the agent hook, with the `pnpm gate:<stage>` to use instead |

## 5. What must not move

- The order: class A, compile, package, qualify, release. No stage before the one it depends on. No stage skipped by a flag, a
  baseline, an environment variable or a hook.
- Deterministic tests are class A. Moving a check out of class A because it is slow is not allowed; make it fast or make it hermetic.
- Executor by host policy: broker on macOS and Windows, direct elsewhere. A broker-managed host whose broker is down refuses; it
  never compiles around the broker. A Linux host never needs a broker, a pin or a launcher.
- One door into the broker: the pinned gate. The broker admits a compile only after the pinned gate's class A passed on the exact
  bytes it compiles.
- The primary checkout, warm cache, one build per app per host.
- No host-specific path in any shared path of the pipeline. Mac and Dell scratch stays on the external drive by their disk policy;
  every other host uses its own scratch root. No `/Volumes/D`, `D:\`, `/opt/homebrew` or `C:\Program Files` in code.
- No anti-forgery layer. The gate's receipt is the gate's word; GitHub verifies the bytes it publishes itself. A broker signature is
  evidence when present, never a prerequisite.
- Publication only from `main`, only of bytes a qualification run sealed, only after that run is green.
- Nothing is called done, proven, green or clean without the receipt or log that shows it.

## 6. Failures not to repeat (10-18 to 10-28)

- The requirement lived in chat; the tests tested the patch. The suite was green on the Mac while publication from Linux was
  impossible, and nobody ran it there.
- Reviewer findings were implemented as new mechanisms without checking them against the requirements: broker-signed receipts made
  publication broker-only; moving tests to class B broke "class A first".
- Proofs were synthetic: a green admit and a red refusal from a hand-made broker request, never a real app build from the entry.
  The calling-gate mismatch and the missing pnpm adapter were found hours after each pin was declared proven.
- Two halves shipped separately and installed on live machines before running together: a broker requiring a variable the gate did
  not send; a Dell cut over with no build proof and no rollback, wedging a checkout.
- One release absorbed every finding: ten pins in two days, eleven review rounds on one pin, nothing shipped.
- Paths of the machine at hand were hardcoded under deadline and never removed.
- Reframing a rule to proceed: a nine-app clone called "not a build"; a release run started over a NO GO because "starting is not
  publishing"; dev installs that replaced running apps, self-authorised.
- Claims ahead of evidence: "every suite green" at 64 of 65, "clean" with 31 commits ahead, "proven" with no receipt.

## 7. Corrective plan, in order

Each step names its acceptance. A step is done when its tests pass on this Linux host and, where a host is named, its receipt exists.

1. **Restore class A.** `test:npm` and `test:scripts-ci` return to class A under the launch guard; the test that was changed to expect
   execution after compile is reverted. Acceptance: a red deterministic test stops the gate before any compile scan starts.
2. **One entry, executor by host policy.** `rightkit-gate` picks broker on macOS and Windows, direct elsewhere, regardless of PATH.
   An app's copy on a managed host hands the run to the pinned gate. `rightkit-gate adopt` writes the `gate:*` scripts and hooks;
   rule G23 refuses any other build entry in an app. `rightkit-gate install-host-gate` puts the launcher on a managed host from the
   pin only. Acceptance: the matrix tests for every origin, plus the "wrong gate copy" row of section 4 on the Mac.
3. **Portable release path.** Remove the broker-host requirement and the mandatory signed receipts from the unit release and from
   `publish-admission.mjs` (signatures verified when present). Dispatch to GitHub from any host: through the broker adapter where there
   is one, in-process elsewhere, with the same checks (`main` equals the admitted commit, preflight green). `gh` from PATH; scratch and
   state roots from the host's disk policy, else the host's temp root; the local qualify path on any platform. Acceptance: the Linux
   release cell green in the matrix test and a real `pnpm gate:release` from this Linux host that reaches the GitHub dispatch.
4. **Packaging ownership.** The signed Mac swap helper becomes one prebuilt input used by every pack, local and CI, or the local
   candidate is bound to CI's final sealed bytes by source revision and those bytes are what qualify and publish. Native signing
   stays. Acceptance: a Linux pack and the CI pack of the same commit compare equal, or the binding test proves the sealed bytes are
   the published ones.
5. **Broker adapters as data.** The `rightkit-pnpm`, `rightkit-node` and `rightkit-swift` entries live in rustright as a committed
   config with a test over the exact argv shapes RightKit sends. Acceptance: the Mac and Dell configs equal the committed entries and
   a real `pnpm gate:compile` of an app on each produces broker receipts for the pnpm steps.
6. **Canary placement.** RightKit and public-app compatibility checks run in CI; private-app compatibility checks run on the Mac and
   the Dell and their results are stored against commit and artifact hashes, read by admission, never committed into the source.
   Acceptance: a RightKit release from Linux needs no Mac or Dell at dispatch time.
7. **Nine cells, three layers.** The matrix test covers every cell; one real run per origin per repository kind writes its receipt;
   the Linux headless path is proven end to end: install, run, signed installers, automatic publication.
8. **Then publish 0.6.9** from the Linux host, and move every app to it with `rightkit-gate adopt`.

## 8. Decisions this document records

- Publication needs no broker signature (Adrian, 2026-10-28; supersedes the 2026-10-28 authenticated-qualification change).
- Deterministic tests are class A (Adrian, 2026-10-27 22:10; restores what pin #10 moved).
- The gate contract matrix is changed only by Adrian.
