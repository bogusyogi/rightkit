# rightkit-cloudkit-helper

Neutral macOS CloudKit helper for the Rust `rightkit-sync` engine (`HelperBackend`). One long-lived
process; JSON lines on stdin/stdout (`rightkit-sync-helper/1`, spec in
`crates/rightkit-sync/API.md` section 4); logs to stderr only. It is a thin protocol adapter over
`RightKitCloudSync.CloudKitSyncTransport` (change tags, `CKAsset` mapping, `CKError` classification,
failed-record reporting, account scope `sha256(container|zone|userRecordName)`), so desktop and
iOS share one CloudKit implementation.

```
swift build -c release                    # in this directory
.build/release/rightkit-cloudkit-helper --backend memory            # no iCloud: protocol E2E
.build/release/rightkit-cloudkit-helper --container <id> --zone <name> --subscription <id>
```

Every request must carry `"v":1`; a missing or different version is refused with error kind
`unsupported_protocol` before any command runs (`swift test` in this directory covers it).

`--backend memory` serves an in-process zone (`InMemorySyncTransport`); `rightkit-sync`'s
`tests/helper_e2e.rs` runs the real engine against this binary when it is built.

## Manual real-CloudKit gate (needs an iCloud account; not run by the build)

Real CloudKit cannot be exercised headlessly. Gate for each consuming app (record the result in that
app's release evidence):

1. Package: copy the binary into `<Helper>.app/Contents/MacOS/`, give the bundle an `Info.plist`
   (own bundle id under the app's team), embed the app's provisioning profile as
   `Contents/embedded.provisionprofile`, and sign with the app's identity and
   `entitlements.template.plist` filled in. A bare Mach-O is killed at launch (donor finding).
   Signing and notarization run on the native host through `docs/rules/release-signing.md`.
2. On a Mac signed into a TEST iCloud account (development container first), run the app's
   `HelperConfig::cloudkit(container, zone, subscription)` engine with its mapper and check:
   a. first pass on an empty zone: zone and subscription created, `status` reports `ready` and a
      64-hex `scope`;
   b. publish more than 200 items, relaunch: the next pass uploads nothing;
   c. edit on a second device (iPhone or Mac), both directions: later `updatedAt` wins, equal
      versions never re-upload;
   c2. race: edit the same item on two devices within seconds (second device offline until just
      before its pass): the conditional save reports a conflict, the newer `updatedAt` survives on
      both, and no edit is silently overwritten;
   d. delete on one device: tombstone arrives, local row removed, content fields cleared, no
      re-import after relaunch;
   e. airplane mode mid-pass: pass fails retryable, ledger cursor unchanged, resumes on reconnect
      honouring at least the server `retry_after`;
   f. sign out, then sign in as another account: pass reports unavailable, then the ledger resets
      and no pending deletion of the old account is written to the new one;
   g. Production schema: every record type and field the mapper writes already exists there (the
      helper cannot create fields in Production).
3. Evidence: helper stderr, the engine `PassReport`s, and `ledger.scope` and cursor readbacks per
   step.
