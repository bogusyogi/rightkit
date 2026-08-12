# Windows Phase-0 evidence

Run `win "Set-Location 'D:\\Claude\\tools\\rightkit\\packages\\build-control-phase0\\windows'; node src\\cli.mjs spike 'D:\\Claude\\tools\\rightkit\\docs\\plans\\evidence\\rightkit-build-control-plane-v5\\windows\\phase0-report-windows.json'"` from macOS. It is inventory-only: it does not deploy App Control, alter boot state, mount/create VHDX, or enable UMCI.

`catalog`, `package`, & `benchmark` generate a read-only Python/Node/Rust native-module catalog, deterministic 2x supplemental-policy load input, & local packaging benchmark. Their policy package is explicitly `NOT_DEPLOYED`; activation, recovery, & timing admission require a sacrificial Windows ring.

Enforced-policy, recovery, supplemental-policy, protected-chamber, & admission-SLA spikes require a sacrificial Windows machine/ring, local administrator operator, recovery policy, & controlled native-module catalog. Record only output from that environment in this directory.
