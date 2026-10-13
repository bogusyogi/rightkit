# M8 rightkit-fs owner gaps: what is built, what needs a decision

Source: `private-audits/inventory/preserve/M8.md` section (c) O1 to O10. fs is unpublished 0.4.0.

## Built (additive, in 0.4.0)

| Gap | Delivered | Note |
|---|---|---|
| O3 fsynced append | `append_record` | Torn-tail repair and record framing stay in the app; log files route to `rightkit-logs` (M15). |
| O4 achieved durability | `AtomicWriter::commit_reporting`, `atomic_write_reporting`, `WriteReport` | Reports the flush level of the temp file plus `PublicationDurability`. The directory flush is `try_sync_dir` and has no level. |
| O7b unknown vs dead | `sweep_stale_temps_report`, `SweepReport` | Windows counts removals only (`retained_counted == false`). |
| O7a one behaviour | Already done by 1590dbfd | Confirmed-dead-only removal; age never removes. |
| O9 verified copy | `copy_verified` | Needs `digest`. Does not lock the source. |
| O10 owner-only executable | `write_private_executable` | Windows has no execute bit; the caller picks the extension. |

## Needs a design or decision (not built)

- **O1 Swift atomic writer.** Decision: add a `RightKitSwift` file-writer product (atomic replace, create-new, `sync`, file-protection class, directory sync) or declare Swift writers app code. Cost of adding: a second implementation of the temp-name and sweep contract that must stay in step with Rust; `FileProtectionType` is Apple-only and not testable in CI. Recommendation: add only after a Swift consumer lists the exact calls it needs (pulse 19 writes, heardright file protection); until then app code.
- **O2 staged-tree publish.** Design: `publish_dir(stage, dest, PublishOptions { rollback })` = create stage under the destination parent, fill, flush tree, `swap_paths` where supported (macOS `RENAME_SWAP`), else rename-aside then rename-in with a recorded rollback name; a crash between the two renames leaves `dest.rktold-<pid>` that a reaper must know about. Needs a decision on the crash-residue name and its sweep rule (it is a second temp namespace) and on Windows directory replace semantics (`MoveFileExW` cannot replace a non-empty directory). Not built because the residue namespace changes `sweep_stale_temps` rules.
- **O5 unsupported-filesystem lock policy.** Decision: `LockOptions::on_unsupported = Refuse | Unlocked`. `Unlocked` makes mutual exclusion silently absent; ember asked for it. Recommendation: allow it only as an explicit, named opt-in that returns a typed `LockOutcome::Unlocked` so the caller cannot mistake it for a held lock. Needs the owner's call before it exists.
- **O6 Pulse Windows ACL profile.** Decision plus Windows-host work: a second verify profile (accept Administrators and SYSTEM entries, refuse foreign read-allow ACEs) beside the strict owner-only check, never merged into it. Needs a Windows host to measure real DACLs; not writable or testable from this Mac session.
- **O7c process-generation identity.** Design: record process start time with the PID in the temp name or a sidecar (`/proc/<pid>/stat` starttime on Linux, `proc_pidinfo` start time on macOS, `GetProcessTimes` on Windows) and treat a PID whose start time differs as dead. Changes the generated temp name grammar (`generated_temp_pid`), which is a public reservation (`BadName`) and a compatibility break for residue from 0.3.x writers; needs a decision on the grammar and on retaining old-shape names.
- **O8 owner-only disk-cache temps.** Lives in `rightkit-cache` (`disk.rs:61`, `:675-677`), not fs. Decision: flip the default of `owner_only` for private caches or document per consumer; and stop ignoring a sweep failure on open. fs already provides the private temp (`AtomicWriteOptions::private`).
