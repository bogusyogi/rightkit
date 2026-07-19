# Right Suite Third-Party Notice Requirements

**Version 1.0 · Effective July 16, 2026**

Third-party notices are generated for an exact app version, platform, architecture, and packaged dependency closure. A suite-wide generic list is not sufficient.

Each release snapshot must identify:

- app key, version, platform/architectures, generation timestamp, lockfile/Cargo.lock hashes, and generator version;
- every distributed JavaScript package, Rust crate, native library, framework, sidecar, font, model, weight, tokenizer, dataset, media binary, and redistributable;
- component name/version, copyright notice when required, license identifier, source URL, modifications, and whether source/NOTICE text or an offer is required;
- full license and NOTICE text in the bundle where the license requires it;
- attribution in the app/site where required by Creative Commons or a supplier term; and
- any copyleft, source-offer, patent, advertising, attribution, trademark, or redistribution condition requiring action.

The generator must use the final package lock, Cargo lock, feature/target graph, bundled-resource list, and sidecar/model manifest. Development-only dependencies are excluded unless shipped. Target-specific dependencies are evaluated per target.

The release fails if a shipped component has a missing/unknown license, a required license/NOTICE file is absent, an attribution is incomplete, a source-offer obligation is unsatisfied, a license conflicts with proprietary distribution, or the notices still contain a template marker or unresolved release checklist.

The proprietary EULA and Ownership Notice must expressly exclude Third-Party Components. They must not add restrictions to a component where its license forbids additional restrictions. Changes or conversions of Creative Commons material must be identified without implying endorsement.

RightKit packages need their own declared package licenses before public distribution. A proprietary product may use an open-source RightKit package, but its EULA cannot silently convert that package into proprietary material.
