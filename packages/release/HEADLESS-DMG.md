# Headless macOS DMGs

`right-release` macOS build, prePackage, package, & development children receive
`CI=true` & `TAURI_BUNDLER_DMG_IGNORE_CI=false`, applied after command environment
overrides. Managed PATH starts with codesign & osascript wrappers; osascript exits
125 immediately with `MAC_AUTOMATION_DENIED`, caller command/PID, & JSON event.
Managed bash wrapper also rejects `bundle_dmg.sh` without `--skip-jenkins`,
covering its env-bash shebang & legacy app-side bash launch before any AppleScript.

MailRight's installed Tauri CLI is 2.11.4. Its source pins bundler 2.9.4:
[bundler manifest](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/Cargo.toml#L1-L3).
Bundler checks literal `CI=true` & adds `--skip-jenkins` unless
`TAURI_BUNDLER_DMG_IGNORE_CI=true`:
[dmg/mod.rs, lines 160–168](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/src/bundle/macos/dmg/mod.rs#L160-L168).
Its shell script skips Finder cosmetic AppleScript when this flag is set:
[bundle_dmg, Finder branch](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/src/bundle/macos/dmg/bundle_dmg#L450-L487).
This preserves DMG creation but omits Finder layout. Direct calls to that shell
script do **not** inspect CI. It invokes absolute `/usr/bin/osascript`, which
bypasses osascript PATH wrapper. Bash guard fails these legacy launches fast;
replace app hooks before SSH resealing. Absolute `/bin/bash` launches bypass
PATH guards & must also migrate.

## Branded builder

```sh
pnpm exec right-release dmg build \
  --app '/path/to/MailRight.app' \
  --out '/path/to/MailRight_0.1.0_universal.dmg' \
  --volname MailRight \
  --background src-tauri/dmg/background.tiff \
  --icon-size 128 \
  --app-position 158,231 \
  --applications-position 492,231 \
  --window 660,400
```

Defaults: 128px icons, app at 158,231, Applications at 492,231, 660×400 window.
PNG & existing Right Suite TIFF backgrounds are accepted. No background yields
plain icon layout. Native macOS only; output ends in `.dmg`.

Builder uses `ditto` to preserve app bytes, symlinks, resource forks, & extended
attributes; adds `/Applications` symlink & `.background/<filename>`; creates HFS+
UDRW via hdiutil; mounts with `-nobrowse -noautoopen`; writes `.DS_Store`; detaches;
converts to UDZO; verifies image; replaces output only after success. Stage
includes 64KiB layout reservation so image sizing accounts for generated records. Failed
builds preserve prior output. Failed detach preserves work directory & emits
`dmg-cleanup-failed` instead of removing mounted content.

Generated `Bud1` allocator/B-tree records contain `bwsp` window bounds, `icvp`
binary plist (icon size/background), `icvl=icnv`, `vSrn`, & per-item `Iloc`
coordinates. Background Alias v2 uses mounted HFS+ CNIDs, creation times,
Unicode names, & volume-relative paths. Formats follow
[ds_store](https://github.com/dmgbuild/ds_store/tree/main/src/ds_store),
[mac_alias](https://github.com/dmgbuild/mac_alias/blob/main/src/mac_alias/alias.py), &
[dmgbuild layout records](https://github.com/dmgbuild/dmgbuild/blob/main/src/dmgbuild/core.py).
Signing, notarization, stapling, updater signatures, & sealing remain existing
release phases. Builder neither signs nor uploads.

## App migration

MailRight `scripts/mac-dmg.mjs` / `scripts/lib/branded-dmg.mjs` should replace
`bash bundle_dmg.sh` with `pnpm exec right-release dmg build` using resolved
`appPath`, `dmgOut`, product name, background, & config coordinates as above.
Remove temporary app staging; RightKit stages internally. Change initial Tauri
`--bundles app,dmg` to `--bundles app`, since shell-script generation is no longer
needed. Preserve managed-artifact mirroring & updater preparation/finalization.
Apply same migration to HeardRight & other hooks invoking `bundle_dmg.sh`.
This task changes no app repositories or package versions.

## SSH proof commands for lead

Run from Mac through Windows bridge. These exercise current RightKit source
without installing or publishing it. First command builds branded image from
installed MailRight bundle; second exercises native fixture image & mounted
layout readback. Both run under plain ssh, without GUI routing.

```sh
win "ssh mac 'node /Volumes/D/claude/tools/rightkit/packages/release/cli/right-release.mjs dmg build --app /Applications/MailRight.app --out /private/tmp/MailRight-headless-proof.dmg --volname MailRight --background /Volumes/D/claude/mailright/src-tauri/dmg/background.tiff --icon-size 128 --app-position 158,231 --applications-position 492,231 --window 660,400'"

win "ssh mac 'cd /Volumes/D/claude/tools/rightkit/packages/release && TMPDIR=/private/tmp RIGHTKIT_TEST_REAL_DMG=1 node --test --test-name-pattern=real.native dmg-build.test.mjs'"
```

After app migration & lead's normal release-input acceptance, reseal MailRight
end to end from primary checkout (starts at prepackage, then existing signing,
notarization, updater, hardening, & seal; no upload):

```sh
win "ssh mac 'cd /Volumes/D/claude/mailright && node /Volumes/D/claude/tools/rightkit/packages/release/cli/right-release.mjs build --config right-release.config.mjs --platform mac --resume-from prepackage'"
```

Package hooks must resolve migrated `right-release dmg build` implementation.
Use lead's approved package adoption path; source CLI alone does not replace
app-local installed `@rightkit/release` invoked by nested `pnpm exec` hooks.
If an existing seal already matches HEAD & cache identity, build verifies it &
returns `resumed`; require fresh `sealed` output to prove full reseal execution.
Verify emitted release ID against current MailRight HEAD; 416faf1 is incident
reference, not authorization to reset checkout. No command above publishes.

## Artifact lease ownership

Active markers retain schema 1 & now add `ownerHost` & `ownerStartTime` beside
`ownerPid`. Same-host dead PIDs & start-time mismatches reclaim immediately,
even before two-hour expiry, emitting `artifact-lease-reclaimed` with old/new
owner & reason. Live matching owners remain protected after expiry. Legacy
markers reclaim dead PIDs; live legacy owners stay protected. Other hosts &
unavailable start-time inspection retain conservative lease behavior.
macOS uses UTC/C-locale `ps lstart`; Windows uses process StartTime UTC ticks.

## Tests

```sh
cd /Volumes/D/claude/tools/rightkit/packages/release
TMPDIR=/private/tmp node --test --test-concurrency=1 --test-force-exit *.test.mjs
TMPDIR=/private/tmp RIGHTKIT_TEST_REAL_DMG=1 node --test --test-name-pattern=real.native dmg-build.test.mjs
```

Fixtures check child CI overrides, typed osascript denials, mocked hdiutil flow,
failed-phase cleanup/output preservation, DS_Store readback, independent Python
stdlib binary-plist parsing, Alias v2 fields, dead PID reclaim, PID reuse, live
owners, legacy markers, & foreign hosts. Native opt-in test builds PNG-backed
UDZO, mounts read-only, checks app/Applications/layout, & detaches.
