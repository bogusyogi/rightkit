# Credentials R2 consumer

Prepared source only. No Cargo command, native run, packaging, registry resolution or publication performed. `Cargo.toml` deliberately contains invalid version placeholders until publisher supplies exact qualified credentials & DB versions; no Cargo.lock supplied.

## Coverage

| Invocation | Hosts / features | Assertions |
|---|---|---|
| `core` | Windows & macOS; default features | PK03-1 HelperTrust validation; typed invalid-team & missing-trusted-helper refusals. macOS also rejects traversal in bundled helper name. |
| `native-helper` | macOS; default features; signed nested helper fixture | PK03-1 `resolve_bundled_login_item`, `new_trusted`, helper readiness through status-only exchange, wrong-team refusal. |
| `native-db` | Windows & macOS; `sqlcipher` | PK03-3/4 R2 example: real durable credential adapter, owned #831 session moves threads, concurrent acquisition refused as Unsettled, stale revision refusal, pending preserved, missing DB classified without creation, explicit create, writer data, complete key rotation, reopen preserves data, pending/recovery cleanup. |
| `native-db` | Windows & macOS; `sqlcipher-system` | Same behavior under application-matched system crypto provider; separate run/target directory. |

`db-session` alone intentionally lacks SQLCipher provider & must not be used for qualification. Selecting both providers intentionally fails compilation. `native-db` fails on other platforms, missing explicit native-store opt-in, guard-denied/unavailable store, wrong scratch root or any assertion failure. It uses production native backend with `GuardMode::Deny`, never MemoryBackend. Native operations are non-interactive. No mode calls OS-auth, helper read/write/delete, or prints key values. Generated keys cross only native Rust credential/SQLCipher APIs.

Native DB success deletes this run's unique synthetic credential slots; DB & lock evidence remain in scratch. Failure preserves namespace & scratch files for recovery; logs print their identifiers before native mutation. Runner owns eventual cleanup after diagnosing failures.

When `GITHUB_ACTIONS` is exactly `true`, `RUNNER_TEMP` & `RK_CONSUMER_RUN_ROOT` must name existing absolute directories. Canonical run root must be a strict descendant of canonical `RUNNER_TEMP`; equal roots, outside paths, prefix siblings & symlink escapes fail before native mutation. Other hosts retain designated `D:/scratch` or `/Volumes/D/scratch` containment policy.

## Versions & sources

Start each R2(a) packed-artifact phase & R2(b) real-registry phase from separate copies of these three files outside repository. Publisher chooses exact candidate numbers from current registry evidence; replace `__CREDENTIALS_VERSION__` & `__DB_VERSION__` including optional DB dependency even for default-feature runs. Both direct dependencies use exact registry versions; there are no path/Git/workspace/patch dependencies.

PowerShell hydration, from disposable consumer directory, with approved versions exported as `RK_CREDENTIALS_VERSION` & `RK_DB_VERSION`:

```powershell
$manifest = Get-Content -Raw -LiteralPath Cargo.toml
$manifest = $manifest.Replace('__CREDENTIALS_VERSION__', $env:RK_CREDENTIALS_VERSION).Replace('__DB_VERSION__', $env:RK_DB_VERSION)
[IO.File]::WriteAllText((Join-Path (Get-Location) 'Cargo.toml'), $manifest)
```

macOS hydration:

```sh
python3 - <<'PY'
import os
from pathlib import Path
p = Path('Cargo.toml')
p.write_text(p.read_text().replace('__CREDENTIALS_VERSION__', os.environ['RK_CREDENTIALS_VERSION']).replace('__DB_VERSION__', os.environ['RK_DB_VERSION']))
PY
```

Reject empty, unresolved or non-exact versions before invoking Cargo. Inspect ambient Cargo configuration to exclude hidden path overrides, `[patch]`, Git replacements & workspace discovery. Publisher records hydrated manifest, package archive SHA-256, native host, compiler, provider, command, exit status & actual output separately.

## R2(a): packed artifacts

Publisher must first prepare complete Cargo directory source under scratch, with exact unpacked candidate archives, required dependency closure & valid `.cargo-checksum.json` files derived from actual artifacts. Credentials adapter requires DB lifecycle candidate in this source; resolving older DB bytes does not qualify #831. Do not alter packed manifests or point dependencies at source checkouts. `RK_R2_DIRECTORY_SOURCE` names absolute directory source. Existing qualified vendoring/registry preparation belongs publisher, outside this consumer.

Windows, from hydrated packed-phase consumer:

```powershell
$env:CARGO_TARGET_DIR = 'D:/scratch/rk-exec-20261020/consumers/credentials-target-packed-win-default'
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$env:RK_R2_DIRECTORY_SOURCE'" run --offline -- core
$env:RIGHTKIT_TEST_REAL_USER_STORES = '1'
$env:RK_CONSUMER_RUN_ROOT = 'D:/scratch/rk-exec-20261020/consumers/credentials'
$env:CARGO_TARGET_DIR = 'D:/scratch/rk-exec-20261020/consumers/credentials-target-packed-win-vendored'
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$env:RK_R2_DIRECTORY_SOURCE'" run --offline --features sqlcipher -- native-db
$env:CARGO_TARGET_DIR = 'D:/scratch/rk-exec-20261020/consumers/credentials-target-packed-win-system'
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$env:RK_R2_DIRECTORY_SOURCE'" run --offline --features sqlcipher-system -- native-db
```

macOS, from hydrated packed-phase consumer:

```sh
export CARGO_TARGET_DIR=/Volumes/D/scratch/credentials-target-packed-mac-default
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$RK_R2_DIRECTORY_SOURCE'" run --offline -- core
export RIGHTKIT_TEST_REAL_USER_STORES=1
export RK_CONSUMER_RUN_ROOT=/Volumes/D/scratch/credentials-r2
mkdir -p "$RK_CONSUMER_RUN_ROOT"
export CARGO_TARGET_DIR=/Volumes/D/scratch/credentials-target-packed-mac-vendored
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$RK_R2_DIRECTORY_SOURCE'" run --offline --features sqlcipher -- native-db
export CARGO_TARGET_DIR=/Volumes/D/scratch/credentials-target-packed-mac-system
rustright cargo --config 'source.crates-io.replace-with="r2-packed"' --config "source.r2-packed.directory='$RK_R2_DIRECTORY_SOURCE'" run --offline --features sqlcipher-system -- native-db
```

Windows system-provider row requires approved OPENSSL_DIR/native toolchain configuration from publishing host. Vendored row requires existing approved OpenSSL build prerequisites. macOS system-provider row uses application-matched native crypto. Missing prerequisites fail qualification; choose neither replacement provider nor skipped success.

## Signed nested helper fixture, each phase

Stage immediately after default-feature build, before `native-db` can overwrite target executable. Use fresh phase-specific scratch paths & supplied signed `HeardRightAppleCredentials.app`; preserve its signature. Consumer uses helper status only.

```sh
export RK_CONSUMER_HELPER_NAME=HeardRightAppleCredentials
export RK_CONSUMER_HELPER_TEAM_ID=6KLGD3LLKF
python3 prepare_signed_helper.py \
  --scratch-root "$RK_APPROVED_SCRATCH_ROOT" \
  --consumer-executable "$CARGO_TARGET_DIR/debug/rightkit-credentials-r2-consumer" \
  --bundle "$RK_CONSUMER_BUNDLE" \
  --receipt "$RK_HELPER_INPUT_RECEIPT"
"$RK_CONSUMER_BUNDLE/Contents/MacOS/consumer" native-helper
```

Fable supplies `RK_SOURCE_HELPER_APP` from approved signed artifact. Script pins helper name & team, verifies source/copy signatures & tree identity, preserves executable bits, rejects path escapes, symlink components, output reuse & overlaps with source/helper app ancestors. Approved scratch root & output parents must exist; bundle & receipt must be fresh strict descendants. Script never signs or authenticates.

Repeat staging & staged launch for packed & fetched registry phases with separate bundles/receipts. Input receipt records actual helper hashes & default consumer hash; `nativeConsumerVerified: false` remains input-only evidence. Runner records subsequent native exit/output separately. Missing artifact, invalid signature, wrong team or failed readiness refuses qualification. Ordinary `cargo run -- native-helper` cannot resolve nested helper.

## R2(b): published registry

After immutable registry readback confirms exact versions, hydrate fresh consumer copy with readback versions, with no packed-source replacement or lockfile inherited from R2(a). Set separate scratch `CARGO_TARGET_DIR` per row & phase. Run following from registry-phase consumer; native DB environment prerequisites match R2(a).

```sh
rustright cargo run -- core
rustright cargo run --features sqlcipher -- native-db
rustright cargo run --features sqlcipher-system -- native-db
```

These argv work in PowerShell & macOS shells; set target directories using host-specific commands above. Reset target directory to default-feature row before copying its executable into fresh registry-phase helper bundle, then repeat signed-helper commands. Preserve real generated lockfile after resolution; read package source/version/checksum from it & fetched immutable archives before claiming R2(b).

## Integrator wiring

Fable owns directory-source construction, exact version hydration, host prerequisite configuration, provider matrix execution, archive/registry byte binding & evidence collection. Run source once per phase/host/provider; code here emits assertions only, never a publication pass receipt. Baseline helper checks run on both hosts; positive nested signed-helper behavior runs on macOS. A failed or unavailable native row remains failure, with retained recovery fixture.

Separate PK08-7 OS-auth prompt/cancel/timeout/lockout qualification, PK03 crash/fault/relocation scenarios, native store suites, R1 closure, formatting & independent diff review remain publisher-owned. Wire these consumers into R2(a)/R2(b), then attach actual host output to corresponding qualification evidence.
