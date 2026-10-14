# RightKit media characterization

This is the Phase 5 pre-extraction record for HeardRight and ScrapeRight. It
does **not** publish `rightkit-ffmpeg` or migrate either app. It freezes what the
two implementations do today and names every gap that must close before the
shared crate can publish.

Run:

```text
node --test rightkit/characterization/media/characterize.test.mjs
node rightkit/characterization/media/characterize.mjs
```

## Observed state

| Area | HeardRight | ScrapeRight | Result |
|---|---|---|---|
| Windows FFmpeg | staged 7.1.5, SHA-256 `ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d` | same | byte-identical |
| Output | WAV, mono, 16 kHz, PCM s16le | same | parity |
| Arguments | adds `-hide_banner -loglevel error` | minimal conversion args | compatible, not identical |
| Cancellation | none; blocking `Command.output()` | 100 ms poll, direct-child kill + wait | divergent |
| Timeout | none | none | missing |
| Digest gate | none | none | missing |
| License rejection | absence of `--enable-gpl` / `--enable-nonfree` | same | insufficient without approved digest |
| Attribution | generated summary/link | full vendored LGPL text through third-party generator | divergent |

Windows paths currently observed:

- `vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe`
- `heardright/tauri-app-next/src-tauri/resources/runtime/ffmpeg.exe`
- `scraperight/src-tauri/resources/runtime/ffmpeg.exe`

All three are byte-identical. The source archive is FFmpeg 7.1.5 SHA-256
`de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f`;
the build recipe is `vendor/ffmpeg-lgpl/BUILD-NOTES.txt`.

The only recorded Mac hashes are stale FFmpeg 7.1 observations:

- arm64: `3f76a1e4a63bcb7b32181d487f867e585915d40f7a5f979b772a18f0873d3f25`
- x86_64: `14db7f8ddfe1252dc7288e48dc11a2e9fbaf02eb4732d2d73022a8748f1272f1`

The July 5 Mac handoff explicitly requires rebuilding and verifying 7.1.5, so
these hashes are evidence only and do not satisfy the shared-artifact gate.

## Extraction rule

`contract.json` is the gate. `rightkit-ffmpeg` remains unpublished and apps stay
unmigrated until all listed gaps close. The eventual shared implementation must
use an immutable signed artifact catalog, verify SHA-256 before staging and
execution, fail closed in installed builds, reject forbidden configuration,
own timeout/cancellation/process-tree cleanup, preserve the PCM contract, and
stage complete license/provenance materials through Right Release.
