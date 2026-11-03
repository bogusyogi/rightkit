# Ember image stack: where the source of record lives (#1158, decision D1)

Decision (2026-11-03): option 1. The RAW decoder, EXIF/XMP/IPTC, colour, lens profile, raster, TIFF and image-codec crates (N1 `rightkit-tiff`, N2 `rightkit-photo-meta`, N3 `rightkit-raw`, N4 `rightkit-color`, N5 `rightkit-lens`, N6 `rightkit-image-codecs`, N11 `rightkit-raster`) stay in Ember as product code under their `lightcraft-*` names. RightKit does no work on them and creates none of those crates now.

## Why

- Single consumer. The issue's own check (grep over the manifests of the ten apps in the 2026-10-24 port order) finds no other app that declares a RAW, EXIF or HEIF decoder. RightKit owns capabilities that more than one app uses; a capability with one consumer is product code, and the issue records that these crates are not RightKit-catalogued, so they are not ownership exceptions.
- Upstream sync. Ember is a fork of storytold/lightcraft and fixes keep landing on its `main` through the desktop agent, mostly in `crates/raw` (31,568 lines: camera calibration, CR3, compressed raws). Option 3 would route every one of those fixes through a port into a published crate and a version bump before Ember sees it. Option 2 adds the publish step without a second consumer. Option 1 leaves the sync path as it is.
- Cost and reversibility. Option 1 costs nothing and blocks nothing ("nothing blocks Ember"). Moving later is a copy of crates that already have crate boundaries, so keeping them in Ember today does not make a later move harder.

## What would reopen it

A second app declaring a need for one of the seven capabilities (a port manifest that lists RAW, EXIF/XMP, HEIF, TIFF, colour or lens handling), or Ember dropping the upstream fork. Then that capability alone is proposed as an add-on with its consumers named. The third-party `image` crate that GenRight, MailRight, ScrapeRight and HeardRight take with different feature sets is a separate question (a shared raster/codec feature set) and is not part of this decision.

## Follow-through

- Ember records the decision in its port notes (`docs/rightkit-port/new-packages.md` rows N1 to N6 and N11: "kept in Ember, #1158") on its next commit; the Ember repository owns that file.
- No `rightkit-ownership.json` change: none of the capabilities is catalogued.
