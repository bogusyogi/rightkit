# ORT/OCR characterization — CodeRight and ScrapeRight

Status: Phase 5 step 1 and internal pre-publication extraction complete on
Windows (2026-07-14). `rightkit-ort` and `rightkit-ocr` are `publish = false`;
this does not publish or migrate either app.

## Reproduce

```powershell
cd D:\Claude\rightkit\characterization\ocr
node --test characterize.test.mjs
node characterize.mjs
```

Outside the standard Windows install paths, pass
`--coderight-model-dir`, `--scraperight-model-dir`,
`--coderight-ort`, and `--scraperight-ort`. The runner decodes the one
committed fixture into a temporary directory, verifies every digest, runs both
real app crates concurrently, compares exact output, and deletes the temporary
file.

## Proven runtime contract

| Item | Result |
| --- | --- |
| Fixture SHA-256 | `803e102100c91a077607cd2d4750d9d748d86ced48fabc0f77062f26dcba0a4b` |
| Detector SHA-256 | `eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1` |
| Recognizer SHA-256 | `5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634` |
| Dictionary SHA-256 | `b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d` |
| Windows ORT SHA-256 | the `onnxruntime` Windows x86_64 record of `crates/rightkit-media/catalog/native-libraries.json` (the single source; the first characterization run used the `onnxruntime-directml` 1.24.4 PyPI wheel file, which the catalog pin replaces) |
| CodeRight output | `RIGHTKIT OCR 2026\nShared parity fixture` |
| ScrapeRight output | exact match |

Both apps use `ort 2.0.0-rc.12` with `api-24` and dynamic loading.
Windows tries DirectML with parallel execution and memory patterns disabled,
then falls back to CPU. Non-Windows builds use CPU. Both use the same detector
threshold (`0.3`), resize/normalization, banding, box extraction and
de-duplication, reading-order grouping, crop/recognition, and greedy CTC
decoding.

The following implementation files are byte-identical:

| File | SHA-256 |
| --- | --- |
| `src/support.rs` | `7e1211f897344360cc8a56b4b358a5ab20f9b92f0693ea6a2146a30ba57aae65` |
| `src/support/boxes.rs` | `5b32f5e3c3a3588129b2a22fe76dae880b43eed6508929228cd9e20da5babf6c` |
| `src/support/decode.rs` | `088d66dea9a351ff9f4207242764ad82e5496dba69f0aa16b51dc562f54589ef` |
| `src/support/preprocess.rs` | `eae1cc57aa8b437f01435bc5df966b7103310684a5590d5ae4ae78bbce22c17a` |

## Exact reconciliation

The OCR trees differ in six files:

- `Cargo.toml`: product crate/dependency names and descriptions only.
  ScrapeRight says PP-OCRv5 while the shared implementation header says v6.
- `src/bin/ocr_test.rs`: product crate names only.
- `src/lib.rs`: product docs/errors and ORT dependency names; CodeRight also
  exposes `ocr_image_with_model_dir`, while both engines already expose
  `OcrEngine::load(model_dir)`.
- `src/support/model_files.rs`: app-specific environment variable and default
  data directory only.
- `src/support/session.rs`: CodeRight extracts the DirectML construction into
  a helper; ScrapeRight keeps the same calls inline. Runtime behavior is
  equivalent.
- `src/tests.rs`: product name in one ignored-test comment. CodeRight's
  ignored test currently reads `SCRAPERIGHT_OCR_TEST_IMAGE`; this is an
  app-local test typo and must not enter the shared crate.

The ORT-common trees differ in product-specific environment variables and
data-root resolution. CodeRight uses `dirs::data_dir`; ScrapeRight delegates
to `scraperight_protocol::app_data_root`. The configure-once behavior,
platform filenames, explicit `ORT_DYLIB_PATH` precedence, and candidate
selection are otherwise the same.

## Extraction contract

`rightkit-ort` should own only:

- configure-once dynamic-library selection;
- platform library filename;
- explicit runtime path plus ordered candidate probing;
- no product environment-variable or data-root policy.

Each app adapter supplies its legacy environment variable and canonical
app-data runtime candidate. This preserves ScrapeRight's centralized path
authority and does not force CodeRight to depend on ScrapeRight protocol code.

`rightkit-ocr` should own:

- `OcrEngine::load(model_dir)` and `run(image)`;
- session/provider construction and CPU fallback;
- preprocessing, boxes, reading order, de-duplication, and CTC decoding;
- explicit model-directory input and product-neutral errors;
- the contract fixture and digest/output test.

Each app adapter should retain only default model location, product telemetry,
and orchestration. The shared crate must not download models or assume an app
directory.

## Required before publication

- Run this same harness on macOS with the bundled dylibs and preserve the
  result; Windows parity does not prove CoreML/CPU runtime packaging.
- Resolve and record the authoritative model provenance/version. Source labels
  currently conflict between PP-OCRv5 and PP-OCRv6 even though the deployed
  artifacts are byte-identical.
- Put model and per-OS ORT digests in the signed Right Release artifact catalog.
- After extraction, rerun this contract against both thin adapters before
  deleting copied source.
