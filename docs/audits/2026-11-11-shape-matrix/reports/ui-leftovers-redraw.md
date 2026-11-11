# UI leftovers redraw: workspace-ui, system-ui, ml-lab, qa-driver, google-identity

Branch `redraw-ui-leftovers` (from `claude/busy-hypatia-e30h7l`), 2026-11-11. Each change is taken on the
recommended option of the shape review, pending the owner's confirmation. Rows: `ui-leftovers-redraw.rows.json`.

## Verdicts

| Package | Was | Now | Version |
|---|---|---|---|
| `@rightkit/workspace-ui` | wrong | best | 0.2.0 (unpublished) |
| `@rightkit/system-ui` | wrong | gaps | 0.2.0 (unpublished) |
| `@rightkit/ml-lab` / `rightkit-ml-lab` | wrong | gaps | 0.2.0 (private) |
| `rightkit-qa-driver` | wrong | gaps | 0.7.2, `publish = false` |
| `rightkit-google-identity` | wrong | gaps | 0.3.0, `publish = false` |

Followers (verdict unchanged, gaps): `@rightkit/mail-ui` 0.2.0, `@rightkit/agent-ui-approvals` 0.2.0.

## @rightkit/workspace-ui 0.2.0

Every view imported mail-ui's store, hooks, IPC, icons and account switcher. Now:

- The views and navs render inside `<WorkspaceHostProvider host={...}>`. `WorkspaceHost` holds a typed
  `WorkspaceClient` (35 calendar, tasks, Drive, contacts and Meet calls plus `openExternal`, `saveTextDownload`), `useState` (a selector hook over the
  host's `WorkspaceState` slices with `set*` setters), and host actions: `reportError`, `notify`, `dialog`,
  `savePreference`, `composeTo`, `linkIntoCompose`, `saveDownload`, `openView`, optional `AccountSwitcher`.
- Moved from mail-ui and deleted there: `CalendarNav`, `ContactsNav`, `DriveNav`, `TasksNav`, `useDrive`,
  `calendarUtils`, `WEEKDAYS`, `colorFor`/`initialOf`, the workspace data model (`CalEvent`, `DriveFile`,
  `ContactFull`, `EventForm`, `TaskCreate`, `ContactDupes`, ...). mail-ui re-exports the types and avatar helpers
  from workspace-ui, so each exists once.
- Glyphs from `lucide-react` through a local `Icon`; nothing from the mail app's icon set.
- `@rightkit/mail-ui` 0.2.0 depends on workspace-ui (it was the other way) and supplies MailRight's host,
  `mailWorkspaceHost` / `mailWorkspaceClient`; its `Sidebar` renders the navs over it. Passing the mail store as
  `useState` typechecks unchanged, so the state contract matches MailRight's store field for field.
- Behaviour change: "Email this contact" opens a new message without switching the host to its inbox first.

## @rightkit/system-ui 0.2.0

- Transport injected: `configureSystemHost({ client, apps, subscribe, appName, storagePrefix })`. `api` and
  `appsApi` are the host's `SystemClient` and `AppsClient` (interfaces generated from the old objects' exact
  types, plus `metricsLease`, `metricsHistory`, `activityLease`, `cleanupHistory`, `initialApp`). Events go through
  a typed `SystemEventMap` (index changes, app rows and icons, update jobs, leftovers, window visibility). No
  `@tauri-apps/api` import and no command name is left. Cleanup's private second client over the same five
  commands is gone; its result types merged into the client's.
- Product strings: the seven "Pulse" strings in the views use `appName`; the `pulse.hub.*` keys are
  `storagePrefix` + the view's key.
- Non-system modules out. Deleted (Pulse's hub keeps its own copies; owners are agent-ui-*, `@rightkit/onboarding`,
  `@rightkit/legal-ui`, `@rightkit/send-ui`): Chat and its stylesheet, onboarding flow and gate, legal receipt
  wiring, the onboarding settings backend, `lib/rk` (logging and device clients created at import), the unused
  `shareApi`. Moved to `@rightkit/agent-ui-approvals` `src/remote`, where Pulse's other hub screens are their
  only users: the device-link, remote-approval, remote-unlock, browser-login, secure-box and notch-setting
  clients and types, LauncherSettings, NearbySettings, settingsKit, QR, chatKind, `membrane.css`, `settings.css`.
- Dependencies removed: `@tauri-apps/api`, legal-ui, logs, onboarding, tauri, tauri-shell.
- Gap kept: Monitor and Overview, system views over this client, sit in agent-ui-approvals with Pulse's hub.

`@rightkit/agent-ui-approvals` 0.2.0 follows: imports the moved files locally, defines `SHOW_ONBOARDING` where it
fires it, and needs `configureSystemHost` before Monitor and Overview render. Its `src/remote` is Pulse's hub
(Settings, Overview, Monitor, Devices, pairing, remote approvals, browser logins, secure box) and needs its own
redraw.

## ml-lab (decision 8)

`rightkit-ml-lab` is not a crate: it is the plan's name for `packages/ml-lab` (`@rightkit/ml-lab`). Both rows now
describe one private tooling package, 0.2.0:

| Path | Entry |
|---|---|
| `wer/score-asr-wer.mjs` | bin `rk-asr-wer`, export `./wer`; `--self-test` passes |
| `asr-bakeoff/CONTRACT.md` | the adapters' and scorer's shared input and output contract (the adapters cited it; it was never moved) |
| `asr-bakeoff/adapters/<model>/adapter.py` | ten name-blind adapters, each pinned to a model revision |
| `asr-bakeoff/bundle-qualify.mjs` | bin `rk-bundle-qualify`, export `./bundle-qualify` |
| `coreml/` | Parakeet TDT/RNN-T conversion, decoder precision matrix, Mac benchmark, Silero VAD and Whisper conversion |
| `requirements/asr-adapters.txt`, `requirements/coreml.txt` | Python manifests (lower bounds the scripts state; exact pins are an open action) |

Machine paths removed: adapter defaults were `parents[5]`/`parents[2]` of the script (outside the package) and are
now `./.cache/asr-models/<model>` and `./out/<model>`; the VAD converter took HeardRight's `src-tauri/resources`
and now takes the ONNX path; the scorer hashed a HeardRight Python file and scored six HeardRight terms in a
`scripts_md` group, now `--terms`/`--term-group`. Removed from RightKit (they stay with HeardRight): 17 dated
`bakeoff/` folders, `scripts/local-model-training`, `local-model-screening`, `l1-polish-replay`,
`bias-experiments`, `parakeet-v3-streaming`, `sotto-deployment-gate`, the unified Parakeet and Windows EOU
scripts, `tools/` wake-word and corpus scripts, the bake-off control runner, report, acquisition roster,
productivity-corpus builders and Cohere smoke scripts, and the CoreML diagnostics tied to HeardRight's model
registry. 298 files became 30 (about 46,000 lines removed).

## rightkit-qa-driver (decision 9)

Not in the mandatory set (checked `packages/release/ownership-mandatory-set.json`). `publish = false` from 0.7.2:
0.7.0 stays on crates.io, no new version is published; README and CHANGELOG say so and point to `rightkit-qa` and
`rightkit-automation`. Deleted: `src/keys.rs` and `src/json.rs` (never compiled; `keys` and `json` already
re-export rightkit-automation's), `registration.patch`, the registry recipe and inventory entry, the
`stagedCargo` feed entry. Kept: the re-exports, the WebDriver bridge and the macOS CLI, which rightkit-qa does not
have and which the e2e app's `qa_driver` probe and the `rk2-macos` spike use.

## rightkit-google-identity (decision 9)

Everything it held is in `rightkit-oauth` (feature `google`), which `rightkit-google-apis` uses: loopback PKCE and
code exchange (`OAuthClient`, `OAuthConfig::google`), ID-token verification (`GoogleIdVerifier` with cached keys,
`verify_with_jwks`), a refreshing session over the credential store (`GoogleSession`), `DRIVE_APPDATA_SCOPE`,
`GOOGLE_ISSUERS`. Its own `IdTokenProvider` seam was never implemented; platform code passes an ID token to
`GoogleIdVerifier::verify`. So the copies are deleted (498 lines of `lib.rs`, 166 of `rsa.rs`), the crate only
re-exports `rightkit_oauth::google`, and it is `publish = false` from 0.3.0 (0.1.0 stays on crates.io). Recipe,
inventory entry and `stagedCargo` entry removed. The e2e app's `google_identity` probe, which repeated the
`oauth` probe's RFC 7636 vector, is deleted with its dependency (e2e lock, notice line 4 and legal manifest
digest updated).

## Recipes run

npm recipes ran against `pnpm pack` archives in a scratch directory with the `RIGHTKIT_CONSUMER_*` environment;
unpublished closure members were overridden with packed archives in the scratch `pnpm-workspace.yaml`:

| Recipe | Result | Overrides |
|---|---|---|
| `@rightkit/workspace-ui` 0.2.0 | ok | none |
| `@rightkit/mail-ui` 0.2.0 | ok | workspace-ui 0.2.0 |
| `@rightkit/system-ui` 0.2.0 | ok | none |
| `@rightkit/agent-ui-approvals` 0.2.0 | ok | workspace-ui, mail-ui, system-ui 0.2.0 |

The workspace-ui and system-ui closures pin `@rightkit/react-shell` 0.7.3, `@rightkit/settings-ui` 0.7.1 and
`@rightkit/platform-ui` 0.4.1 (react-shell's peer), the versions the packed manifests resolve on the registry
today; the old pins (0.7.2, 0.7.0) installed beside 0.7.3 and the root import failed on react-shell's
`platform-ui` peer. The two crate recipes are removed with the publication.

## Checks

- Crates: `cargo +1.99.1 clippy --all-features -D warnings` and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --all-features` for both crates, `rustfmt --check`, `cargo metadata --locked` (root and e2e-app).
- npm: typecheck and build of workspace-ui, mail-ui, system-ui, agent-ui-approvals; ml-lab `test`.
- `node scripts/preflight.mjs --worktree`: 62 rules pass.

## Not done

- `cargo check` of the e2e app's engine (the only dependant of both crates): skipped for disk space (under 6 GB
  free on the shared machine). The change there is the deleted probe module, its registration line and its
  dependency; qa-driver's public paths are unchanged.
- system-ui: Monitor (and Overview, if system UI) into this package; agent-ui-approvals `src/remote` (Pulse's hub)
  needs its own redraw, and its NearbySettings is the second nearby-sharing UI the send-ui row asks to remove.
- ml-lab: exact Python pins from a verified run; one adapter and the CoreML conversion run end to end.
- qa-driver: decide on the WebDriver bridge and CLI, then move the e2e probe and spike and delete the crate (and
  its `ci/gate.config.json` entry, whose `allowIgnored` names test files that no longer exist).
- google-identity: move capability `google-identity` to `rightkit-oauth` through the ownership approval, then
  delete the crate.
- mail-ui's own actions (a `MailClient` seam, agent and licence IPC out, mocks to a dev entry) are unchanged.
