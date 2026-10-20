# Native filesystem qualification

`qualify-fs.yml` takes an explicit main `publish.yml` run. Guard, plan, pack & both native verify jobs must succeed; final seal-check may fail for an unapproved candidate. GitHub artifact digests, sealed manifest hashes & host receipts bind every consumed archive. This standalone consumer pins `rightkit-fs =0.4.6`; registry-consumer builds it from those exact sealed bytes through an offline directory registry.

Windows requires isolated NTFS & FAT32 VHDs. macOS requires an APFS sparse image. Images & mountpoints live under runner temporary storage outside checkout. Setup fails when native mounting or filesystem identification fails. Always-run cleanup detaches volumes before recursive removal & emits its own receipt.

Consumer requires distinct native volume identities, two staged files, committed readback, unchanged candidates, rollback & retained-journal recovery. Archived crate's existing cross-volume test also runs exactly once with its mandatory fixture supplied.

FAT32 cannot represent Windows owner-only DACLs. Private publication is therefore explicitly unsupported: construction must return an error without publishing destination. A separate raw FAT32 file records actual strict-sync result; this does not claim private publication support. NTFS & APFS private publication must succeed. Qualification artifacts retain native volume reports, consumer command output, strict-sync result, source identities & cleanup receipt.
