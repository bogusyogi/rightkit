# Mac ownership helper validation

Schema-v2 source update: exact payload keys, fingerprint ID regex, policy digest, dotted-numeric scanner/catalog floors, 1–256 distinct path/hash occurrences, issued/expiry window capped at 90 days, & verified HTTPS tracking without userinfo. Exception signing emits schema 2; key-log schema stays 1. Run `node --test rightkit/ownership-approval/conformance/native-source-conformance.test.mjs` from workspace root on macOS or Windows. This consumes shared `conformance/v2-vectors.json`, pins native refusal guards & checks source-projected rules. This update has source checks only; native evidence below describes prior build.

Checked 2026-10-08 on macOS 27.0.1, native arm64.

Helper: `/Users/adrdsouza/.rightkit-managed/ownership-approval/macos/content-2c30864bdb666a1ca1e4388ea7a3bc83cb985196d666b96cc30e1da7cd5b0e6b/RightKitOwnershipApproval.app/Contents/MacOS/ownership-approval`

Executable SHA-256: `2c30864bdb666a1ca1e4388ea7a3bc83cb985196d666b96cc30e1da7cd5b0e6b`

Developer ID: `Adrian D'souza (6KLGD3LLKF)`; bundle ID: `com.orthiclabs.rightkit.ownership-approval`. Whole signed app bundle was copied into its content-addressed directory; executable digest & strict signature verification were read back after staging. No provisioning profile or restricted entitlements are present.

| Acceptance | Evidence | Result |
|---|---|---|
| Profile-free hardware key creation/restoration | Ephemeral Secure Enclave P-256 key with userPresence/privateKeyUsage created & restored without entitlements; nothing persisted | PASS |
| Presence cannot be bypassed | Same ephemeral key refused signing when LAContext interaction was disabled: LocalAuthentication -1004, user interaction required | PASS |
| Signed helper storage access | Native helper added & read UUID-only generic-password sentinel using its trusted-app ACL in unlocked default user Keychain | PASS |
| Unrelated executable rejection | Independent executable confirmed Keychain unlocked, then read failed with errSecAuthFailed (-25293), exit 1; no sentinel bytes returned | PASS |
| Helper update continuity | Different executable bytes signed with same Developer ID/bundle identity read sentinel silently with READ_ALLOWED, exit 0 | PASS |
| Test cleanup | Sentinel deleted successfully; follow-up query returned errSecItemNotFound before PASS output | PASS |
| Tamper rejection | Modified app Info.plist failed strict signature verification, exit 1; preflight did not succeed | PASS |
| Staged behavior | Staged helper preflight & software crypto/key-log checks passed in UID 501 GUI session | PASS |
| Cross-platform verification | Production JS verifier accepted staged Mac ECDSA output & signed root/log addition containing Windows RSA public key | PASS |
| Ceremony & chain regressions | Ownership command, key-log, trust & authority tests: 35 passed, 0 failed; source conformance keeps one LAContext/private operation & restores Keychain interaction before signing | PASS |
| Full release suite on Windows | 793 tests: 748 passed, 39 failed, 6 skipped; 462.512 seconds | FAIL |
| Real enrollment & interactive prompt count | Reserved for Adrian; no ownership key enrolled, native authentication prompted or protected trust/checkpoint changed | NOT RUN |

Full-suite failures include Mac-only fixtures using /private/tmp or POSIX uid/mode checks on Windows, Apple-profile mocks, signing-session fixtures & existing app release-contract checks. Full-suite success is not claimed. Native validation logs remain in build directory `/Users/adrdsouza/.rightkit-managed/ownership-approval/macos/build-8e2eb3dcbb7b476a83a19ee38d288c3c/`; Windows test output is in `.audit/rightkit-local-storage-release-tests.log`.

Local storage uses legacy Keychain ACLs, independently of Secure Enclave presence enforcement. An explicit macOS user grant can authorize another application; helper reads never request that grant or retry authentication. Verified update continuity preserves normal one-command enrollment/approval flow.
