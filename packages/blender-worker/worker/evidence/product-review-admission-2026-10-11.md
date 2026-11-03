# Product review admission verification

Windows, Blender 5.2.2 LTS. [Portable results & source hashes](product-review-admission-2026-10-11.json). Synthetic triangle, synthetic shader/reference & explicitly synthetic owner records only. No Fenrir spec or swatch has been approved.

- Python contract suite: 22 passed; two Blender-only modules skipped under ordinary Python.
- Native Blender suite: two test methods passed; 22 real child CLI jobs & four direct assertions. Covers primitive swatch generation before separate approval, reviewed hero output/provenance, finalized reuse, source-free probe, exact imported mesh/slot matching, missing/rejected/stale inputs, override rejection, nested unpacked textures & material-name collisions.
- Ten fresh rejected jobs emitted no PNG or final result manifest. Stale finalized reuse also failed, preserving prior artifacts instead of accepting them under changed inputs.
- Freshly built GenRight engine: missing reviews fail through real job runner with no frames; reviewed synthetic hero/two films reach verified library adoption, retained checkpoints, review-bound regeneration & cancellation. Both E2E invocations passed. Managed build/test request IDs are in JSON.

Reproduce worker checks:

```powershell
py -3.11 -m unittest discover -s tests -v
& 'C:/absolute/blender.exe' -b --factory-startup --python-exit-code 1 --python tests/test_product_review_blender.py -- 'D:/absolute/fresh-evidence-directory'
```

GenRight: `rightkit cargo build -p genright-engine`, then `rightkit cargo test -p genright-e2e --features local-models --test local_blender -- --test-threads=1`. Pin `GENRIGHT_ENGINE` to resulting managed build & `E2E_BLENDER_WORKER_DIR` to reviewed worker checkout in ignored E2E settings; existing settings override defaults. Without review settings, suite exercises rejection. Positive path uses generated synthetic fixture paths from native summary as `E2E_BLENDER_SOURCE`, `E2E_BLENDER_PRODUCT_SPEC` & `E2E_BLENDER_SWATCH_APPROVAL`. Do not create approvals for real products to make tests pass.

This verifies source implementation, not an installed-app update. Local review records remain trusted operator evidence of human decisions. Visual quality is independent: earlier Fenrir imagery remains rejected for lighting, composition & surface realism. See [visual review](fenrir-lookdev-visual-review-2026-10-11.md).
