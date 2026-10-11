# Performance lens notes — 2026-10-11

Packet coverage is incomplete: 29 cache-dominated paths are examined and 1,967 denominator paths are omitted by the total-byte cap. Result remains `complete:false`; no runtime scale, RSS, CPU, or throughput claim is made.

Fresh runtime-budget review confirms no current named installed-app scale run. Findings therefore use only live source behavior and recommend measurement before setting bounds.

Resilience overlap was checked independently. The cited fsindex FSEvents ingress, rightkit-watch subscriber/raw ingress, service Unix pre-hello threads, and jobs subscriber queues have no source-level capacity or coalescing bound in current code. They remain owned by resilience result and are not repeated here; performance findings cover distinct full-state, full-sort, full-enumeration, flush, and announcement-worker costs.
