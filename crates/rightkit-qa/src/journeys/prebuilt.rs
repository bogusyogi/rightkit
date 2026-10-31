//! Prebuilt journey input (audit GB-79; GATE test stage E1/E6/E11): `journeys run` consumes a build the GATE already carried.
//!
//! On a snapshot stage the gate is the only compiler: it carries `cargo test ... --no-run --message-format=json` through its
//! carrier (one signed, ledgered broker request), signs the products that must be signed, and hands `journeys run` the carried
//! listing together with an attestation of what was carried. `journeys run --from-cargo-json <listing> --prebuilt-attestation
//! <file>` then verifies and consumes them WITHOUT compiling: [`build_from_prebuilt`] stands in for the broker spawn
//! ([`super::build_record::spawn_broker`]) and returns the [`RawBuild`] the build record is made from, so every later check (the
//! QA marker, source identity, the launch-time re-hash) runs exactly as for a build the runner spawned itself.
//!
//! The attestation (`rightkit.journeys.prebuilt-input/1`, written by `packages/release/qa-journeys-prebuilt.mjs` from the VERIFIED
//! carried receipt, never from the listing) binds four things:
//!
//! * the broker **request id** and the sha256 of the carried request (the receipt's request digest);
//! * the sha256 of the **listing** bytes: a changed listing is refused (`prebuilt_listing_mismatch`);
//! * the **command** the carried build ran, which must equal the build the journeys manifest declares: an input that was built by
//!   another command, or a hidden second build, is refused (`prebuilt_command_mismatch`);
//! * the sha256 of every **output** the listing names: a changed or unlisted artifact is refused (`artifact_mismatch`,
//!   `artifact_not_from_build`).
//!
//! A product that must carry the QA control marker (every role but the adapter and engine, [`super::build_record::MarkerPolicy`])
//! is launched only as the gate's SIGNED copy: exactly one record of the signing evidence ([`crate::signing::Evidence`]) must name
//! the attested output as its `sha256Before`, the signed copy must still hash to its `sha256After`, and that copy replaces the
//! unsigned path in the listing the record is made from. No evidence, or no such record, is `unsigned_product`.
//!
//! Nothing here spawns a process, and an empty listing is never accepted: a journey set that built nothing cannot be green.
use super::build_record::{
    parse_cargo_json, select_artifact, valid_request_id, BuildPlan, MarkerPolicy, RawBuild,
    BROKER_PROGRAM,
};
use super::{code, is_hex_lower, Problem};
use crate::signing::Evidence;
use crate::util::{err, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The attestation file's schema.
pub const PREBUILT_SCHEMA: &str = "rightkit.journeys.prebuilt-input/1";
/// The capability a gate asks of `rightkit-qa` before it plans a journeys step on a snapshot stage.
pub const PREBUILT_CAPABILITY: &str = "journeys-prebuilt-input";
/// The `rightkit-qa journeys run` options that carry the input.
pub const LISTING_FLAG: &str = "--from-cargo-json";
pub const ATTESTATION_FLAG: &str = "--prebuilt-attestation";

/// Problem codes of a refused prebuilt input.
pub mod prebuilt_code {
    pub const LISTING_MISMATCH: &str = "prebuilt_listing_mismatch";
    pub const COMMAND_MISMATCH: &str = "prebuilt_command_mismatch";
    pub const NOT_ATTESTED: &str = "prebuilt_not_attested";
    pub const UNSIGNED_PRODUCT: &str = "unsigned_product";
    pub const EMPTY_LISTING: &str = "prebuilt_empty_listing";
}

fn coded(c: &str, detail: impl std::fmt::Display) -> Error {
    Error(format!("{c}: {detail}"))
}

/// One output of the carried build and the sha256 the gate verified for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputHash {
    pub path: String,
    pub sha256: String,
}

/// What the gate attests about the build it carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrebuiltAttestation {
    pub schema: String,
    /// The broker request that built it (the carried receipt's request id).
    pub request_id: String,
    /// The sha256 of the carried request.
    pub request_sha256: String,
    /// The sha256 of the listing's bytes.
    pub listing_sha256: String,
    /// The carried cargo argv, without the broker program: `cargo test -p x --no-run --message-format=json`.
    pub command: Vec<String>,
    /// Every executable the listing names, with its verified sha256.
    pub outputs: Vec<OutputHash>,
}

impl PrebuiltAttestation {
    /// Parse and shape-check the attestation file's bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let a: PrebuiltAttestation = serde_json::from_slice(bytes).map_err(|e| {
            coded(
                prebuilt_code::NOT_ATTESTED,
                format!("the prebuilt attestation is not a {PREBUILT_SCHEMA} document: {e}"),
            )
        })?;
        if a.schema != PREBUILT_SCHEMA {
            return Err(coded(
                prebuilt_code::NOT_ATTESTED,
                format!(
                    "the prebuilt attestation schema is {:?}, not {PREBUILT_SCHEMA}",
                    a.schema
                ),
            ));
        }
        if !valid_request_id(&a.request_id) {
            return Err(coded(
                prebuilt_code::NOT_ATTESTED,
                "the prebuilt attestation carries no valid broker request id",
            ));
        }
        for (what, value) in [
            ("request_sha256", &a.request_sha256),
            ("listing_sha256", &a.listing_sha256),
        ] {
            if !is_hex_lower(value, 64) {
                return Err(coded(
                    prebuilt_code::NOT_ATTESTED,
                    format!("the prebuilt attestation's {what} is not a sha256"),
                ));
            }
        }
        if a.command.is_empty() {
            return Err(coded(
                prebuilt_code::NOT_ATTESTED,
                "the prebuilt attestation names no command",
            ));
        }
        if a.outputs.is_empty() {
            return Err(coded(
                prebuilt_code::EMPTY_LISTING,
                "the prebuilt attestation attests no output: a build that produced nothing cannot be a journey run",
            ));
        }
        if let Some(o) = a
            .outputs
            .iter()
            .find(|o| o.path.is_empty() || !is_hex_lower(&o.sha256, 64))
        {
            return Err(coded(
                prebuilt_code::NOT_ATTESTED,
                format!(
                    "the prebuilt attestation's output {:?} has no sha256",
                    o.path
                ),
            ));
        }
        Ok(a)
    }
}

/// The gate's carried build, as handed to `journeys run`.
#[derive(Debug, Clone)]
pub struct PrebuiltInput {
    pub listing: PathBuf,
    pub attestation: PathBuf,
    /// The gate's signing evidence (`RIGHTKIT_QA_SIGNING_EVIDENCE`); `None` when the gate signed nothing.
    pub evidence: Option<Evidence>,
}

impl PrebuiltInput {
    /// The input of a `journeys run` argv: both options or neither. `evidence` is loaded from the environment when it names a
    /// receipt; an unreadable receipt is an error, never "no evidence".
    pub fn from_args(listing: Option<String>, attestation: Option<String>) -> Result<Option<Self>> {
        match (listing, attestation) {
            (None, None) => Ok(None),
            (Some(listing), Some(attestation)) => {
                let evidence = match std::env::var_os(crate::signing::EVIDENCE_ENV) {
                    Some(v) if !v.is_empty() => Some(Evidence::load(Path::new(&v)).map_err(|e| {
                        coded(prebuilt_code::UNSIGNED_PRODUCT, format!("the signing evidence is unusable: {e}"))
                    })?),
                    _ => None,
                };
                Ok(Some(PrebuiltInput { listing: listing.into(), attestation: attestation.into(), evidence }))
            }
            (Some(_), None) => err(format!("{LISTING_FLAG} needs {ATTESTATION_FLAG}: a listing without the gate's attestation is unverified input")),
            (None, Some(_)) => err(format!("{ATTESTATION_FLAG} needs {LISTING_FLAG}")),
        }
    }
}

fn sha256_text(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

/// Verify the carried build against the plan and return what [`super::build_record::record_build`] reads in place of a spawned
/// build. Pure apart from reading the listing, the attested outputs and the signed copies; it never starts a process.
pub fn build_from_prebuilt(plan: &BuildPlan, input: &PrebuiltInput) -> Result<RawBuild> {
    let attestation_bytes = std::fs::read(&input.attestation).map_err(|e| {
        coded(
            prebuilt_code::NOT_ATTESTED,
            format!(
                "cannot read the prebuilt attestation {}: {e}",
                input.attestation.display()
            ),
        )
    })?;
    let att = PrebuiltAttestation::parse(&attestation_bytes)?;
    let listing_bytes = std::fs::read(&input.listing).map_err(|e| {
        coded(
            prebuilt_code::LISTING_MISMATCH,
            format!(
                "cannot read the carried listing {}: {e}",
                input.listing.display()
            ),
        )
    })?;
    let listing_sha = sha256_text(&listing_bytes);
    if listing_sha != att.listing_sha256 {
        return Err(coded(
            prebuilt_code::LISTING_MISMATCH,
            format!(
                "the listing hashes to {listing_sha}, not the {} the gate attested for request {}",
                att.listing_sha256, att.request_id
            ),
        ));
    }
    // The build the gate carried must BE the build the manifest declares (no hidden or other build): the plan's command minus the
    // broker program, which the carrier supplies.
    let declared = match plan.command.split_first() {
        Some((program, rest)) if program == BROKER_PROGRAM => rest.to_vec(),
        _ => plan.command.clone(),
    };
    if att.command != declared {
        return Err(coded(
            prebuilt_code::COMMAND_MISMATCH,
            format!(
                "the gate carried `{}` but the journeys manifest declares `{}`; the input is not that build",
                att.command.join(" "),
                declared.join(" ")
            ),
        ));
    }
    let listing = String::from_utf8(listing_bytes).map_err(|_| {
        coded(
            prebuilt_code::LISTING_MISMATCH,
            "the carried listing is not UTF-8 text",
        )
    })?;
    let parsed = parse_cargo_json(&listing);
    if parsed.build_finished != Some(true) {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "the carried listing has no successful build-finished record",
        ));
    }
    if !parsed.artifacts.iter().any(|a| a.executable.is_some()) {
        return Err(coded(
            prebuilt_code::EMPTY_LISTING,
            "the carried listing names no executable: an empty build is never a journey run",
        ));
    }
    // Every artifact the plan declares: attested, unchanged, and signed when it is a product.
    let mut launch: Vec<(String, String)> = Vec::new(); // (listing path, path to launch)
    for spec in &plan.artifacts {
        let found = select_artifact(spec, &parsed.artifacts)?;
        let exe = found
            .executable
            .clone()
            .ok_or_else(|| Error(format!("role {:?} has no executable", spec.role)))?;
        let exe_text = exe.to_string_lossy().into_owned();
        let attested = att
            .outputs
            .iter()
            .find(|o| o.path == exe_text)
            .ok_or_else(|| {
                coded(
                    code::ARTIFACT_NOT_FROM_BUILD,
                    format!(
                        "{exe_text} (role {:?}) is not an output the gate attested for request {}",
                        spec.role, att.request_id
                    ),
                )
            })?;
        match spec.marker {
            MarkerPolicy::NotApplicable => {
                let actual = rightkit_fs::digest::sha256_file(&exe).map_err(|e| {
                    coded(
                        code::ARTIFACT_MISMATCH,
                        format!("{exe_text} (role {:?}) cannot be hashed: {e}", spec.role),
                    )
                })?;
                if actual != attested.sha256 {
                    return Err(coded(
                        code::ARTIFACT_MISMATCH,
                        format!("{exe_text} (role {:?}) hashes to {actual}, not the {} the gate attested", spec.role, attested.sha256),
                    ));
                }
            }
            MarkerPolicy::Required => {
                // A product that still sits at its build path must be the attested bytes; the launched file is the signed copy.
                if exe.is_file() {
                    let actual = rightkit_fs::digest::sha256_file(&exe).map_err(|e| {
                        coded(
                            code::ARTIFACT_MISMATCH,
                            format!("{exe_text} cannot be hashed: {e}"),
                        )
                    })?;
                    if actual != attested.sha256 {
                        return Err(coded(
                            code::ARTIFACT_MISMATCH,
                            format!("{exe_text} (role {:?}) hashes to {actual}, not the {} the gate attested", spec.role, attested.sha256),
                        ));
                    }
                }
                let Some(evidence) = input.evidence.as_ref() else {
                    return Err(coded(
                        prebuilt_code::UNSIGNED_PRODUCT,
                        format!("{exe_text} (role {:?}) is a product and the gate handed no signing evidence: an unsigned product is never launched", spec.role),
                    ));
                };
                let base = exe
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mut hits = evidence.records().iter().filter(|r| {
                    r.sha256_before.as_deref() == Some(attested.sha256.as_str())
                        && Path::new(&r.file)
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy() == base)
                });
                let record = match (hits.next(), hits.next()) {
                    (Some(r), None) => r,
                    (None, _) => {
                        return Err(coded(
                            prebuilt_code::UNSIGNED_PRODUCT,
                            format!("no signing record names {exe_text} (role {:?}, attested {}): an unsigned product is never launched", spec.role, attested.sha256),
                        ))
                    }
                    (Some(_), Some(_)) => {
                        return Err(coded(
                            prebuilt_code::UNSIGNED_PRODUCT,
                            format!("several signing records name {exe_text} (role {:?}); refusing to pick one", spec.role),
                        ))
                    }
                };
                evidence.require(&record.signed, &record.signed).map_err(|e| {
                    coded(prebuilt_code::UNSIGNED_PRODUCT, format!("the signed copy of {exe_text} (role {:?}) is not the evidence's: {e}", spec.role))
                })?;
                launch.push((exe_text, record.signed.to_string_lossy().into_owned()));
            }
        }
    }
    // The listing the record is made from: the signed copy stands in for each unsigned product path; nothing else changes.
    let stdout = listing
        .lines()
        .map(|line| match serde_json::from_str::<Value>(line.trim()) {
            Ok(mut v) if v["reason"] == "compiler-artifact" => {
                if let Some(path) = v["executable"].as_str() {
                    if let Some((_, signed)) = launch.iter().find(|(from, _)| from == path) {
                        v["executable"] = Value::String(signed.clone());
                    }
                }
                v.to_string()
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(RawBuild {
        code: Some(0),
        stdout,
        stderr: format!("rustright: request {}\n", att.request_id),
        timed_out: false,
        broker: Some(format!("carried:{}", att.request_sha256)),
    })
}

/// The schema of [`plan_report`].
pub const BUILD_PLAN_SCHEMA: &str = "rightkit.journeys.build-plan/1";

/// What a gate needs to carry the build of a journeys run (`journeys run --print-build-plan [--from-cargo-json <listing>]`): the
/// declared build command (the broker program included; the gate carries `command[1..]`), the working directory, and per declared
/// role its marker policy, so the gate knows which roles are products it must sign. Given the carried `listing`, each role also
/// names the executable [`select_artifact`] resolves for it, so the gate signs and attests exactly the files a run will launch.
/// Nothing is created, built or launched.
pub fn plan_report(plan: &BuildPlan, listing: Option<&Path>) -> Result<Value> {
    let parsed = match listing {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                coded(
                    prebuilt_code::LISTING_MISMATCH,
                    format!("cannot read the carried listing {}: {e}", path.display()),
                )
            })?;
            Some(parse_cargo_json(&text))
        }
        None => None,
    };
    let mut artifacts = Vec::new();
    for spec in &plan.artifacts {
        let executable = match &parsed {
            Some(p) => select_artifact(spec, &p.artifacts)?
                .executable
                .as_ref()
                .map(|e| e.to_string_lossy().into_owned()),
            None => None,
        };
        artifacts.push(serde_json::json!({
            "role": spec.role,
            "marker": if spec.marker.required() { "required" } else { "not_applicable" },
            "cargo_target": spec.cargo_target,
            "cargo_package": spec.cargo_package,
            "executable": executable,
        }));
    }
    Ok(serde_json::json!({
        "schema": BUILD_PLAN_SCHEMA,
        "name": plan.name,
        "command": plan.command,
        "cwd": plan.cwd.to_string_lossy(),
        "artifacts": artifacts,
    }))
}

/// The same refusal as a list of problems, for a caller that reports instead of failing.
pub fn problems_of(e: &Error) -> Vec<Problem> {
    let (c, detail) =
        e.0.split_once(": ")
            .unwrap_or(("prebuilt_not_attested", e.0.as_str()));
    vec![Problem {
        code: c.to_string(),
        id: None,
        detail: detail.to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journeys::build_record::{parse_request_id, validate_build_command, ArtifactSpec};

    const REQ: &str = "096d6843-aaaa-4bbb-8ccc-0123456789ab";
    const MARKER: &str = "RIGHTKIT-QA-CONTROL-SERVER-PRESENT";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rkqa-prebuilt-{tag}-{}-{:x}",
            std::process::id(),
            rand_tail()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rand_tail() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// A carried build: the plan, the listing, the attestation, the signing evidence and the files, all on disk.
    struct Fx {
        dir: PathBuf,
        plan: BuildPlan,
        listing: PathBuf,
        attestation: PathBuf,
        app: PathBuf,
        adapter: PathBuf,
        signed: PathBuf,
        input: PrebuiltInput,
    }

    fn evidence_json(file: &str, signed: &Path, before: &str, after: &str) -> Vec<u8> {
        serde_json::json!({"schema": 1, "step": "sign", "files": [{
            "file": file, "signed": signed.to_string_lossy(), "sha256Before": before, "sha256After": after
        }]})
        .to_string()
        .into_bytes()
    }

    fn fx(tag: &str) -> Fx {
        let dir = tmp(tag);
        let target = dir.join("broker-target/debug");
        std::fs::create_dir_all(&target).unwrap();
        let adapter = target.join("journeys_adapter-1234");
        let app = target.join("fxapp");
        std::fs::write(&adapter, vec![1u8; 300]).unwrap();
        let mut bytes = vec![2u8; 300];
        bytes.extend_from_slice(MARKER.as_bytes());
        std::fs::write(&app, &bytes).unwrap();
        let signed_dir = dir.join("signed");
        std::fs::create_dir_all(&signed_dir).unwrap();
        let signed = signed_dir.join("fxapp");
        bytes.extend_from_slice(b"SIGNATURE");
        std::fs::write(&signed, &bytes).unwrap();
        let line = |pkg: &str, name: &str, kind: &str, test: bool, exe: &Path| {
            serde_json::json!({"reason": "compiler-artifact", "package_id": pkg, "target": {"name": name, "kind": [kind]},
                "profile": {"test": test}, "features": ["qa"], "executable": exe.to_string_lossy(), "fresh": false})
            .to_string()
        };
        let listing_text = [
            line(
                "path+file:///x/fx#fx@0.1.0",
                "journeys_adapter",
                "test",
                true,
                &adapter,
            ),
            line(
                "path+file:///x/crates/fxapp#0.1.0",
                "fxapp",
                "bin",
                false,
                &app,
            ),
            r#"{"reason":"build-finished","success":true}"#.to_string(),
        ]
        .join("\n");
        let listing = dir.join("listing.jsonl");
        std::fs::write(&listing, &listing_text).unwrap();
        let command =
            validate_build_command(&argv("rustright cargo build -p fxapp --features qa")).unwrap();
        let plan = BuildPlan::new(
            "fx",
            &command,
            dir.clone(),
            vec![
                ArtifactSpec {
                    role: "app".into(),
                    cargo_target: None,
                    cargo_package: Some("fxapp".into()),
                    marker: MarkerPolicy::Required,
                },
                ArtifactSpec {
                    role: "adapter".into(),
                    cargo_target: Some("journeys_adapter".into()),
                    cargo_package: None,
                    marker: MarkerPolicy::NotApplicable,
                },
            ],
        )
        .unwrap();
        let sha = |p: &Path| rightkit_fs::digest::sha256_file(p).unwrap();
        let att = PrebuiltAttestation {
            schema: PREBUILT_SCHEMA.into(),
            request_id: REQ.into(),
            request_sha256: "ab".repeat(32),
            listing_sha256: sha256_text(listing_text.as_bytes()),
            command: command[1..].to_vec(),
            outputs: vec![
                OutputHash {
                    path: app.to_string_lossy().into(),
                    sha256: sha(&app),
                },
                OutputHash {
                    path: adapter.to_string_lossy().into(),
                    sha256: sha(&adapter),
                },
            ],
        };
        let attestation = dir.join("attestation.json");
        std::fs::write(&attestation, serde_json::to_vec(&att).unwrap()).unwrap();
        let evidence = Evidence::parse(&evidence_json(
            "debug/fxapp",
            &signed,
            &sha(&app),
            &sha(&signed),
        ))
        .unwrap();
        let input = PrebuiltInput {
            listing: listing.clone(),
            attestation: attestation.clone(),
            evidence: Some(evidence),
        };
        Fx {
            dir,
            plan,
            listing,
            attestation,
            app,
            adapter,
            signed,
            input,
        }
    }

    fn refused(r: Result<RawBuild>, c: &str) -> String {
        let e = r.expect_err("must be refused");
        assert!(e.0.starts_with(&format!("{c}: ")), "wanted {c}, got {e}");
        e.0
    }

    #[test]
    fn a_verified_carried_build_is_consumed_without_a_spawn_and_launches_the_signed_copy() {
        let f = fx("ok");
        let raw = build_from_prebuilt(&f.plan, &f.input).unwrap();
        assert_eq!(raw.code, Some(0));
        assert!(!raw.timed_out);
        assert_eq!(
            parse_request_id(&raw.stderr).unwrap(),
            REQ,
            "the build is tied to the attested broker request"
        );
        let parsed = parse_cargo_json(&raw.stdout);
        assert_eq!(parsed.build_finished, Some(true));
        let app = parsed
            .artifacts
            .iter()
            .find(|a| a.target == "fxapp")
            .unwrap();
        assert_eq!(
            app.executable.as_deref(),
            Some(f.signed.as_path()),
            "the product is launched as the signed copy"
        );
        let adapter = parsed
            .artifacts
            .iter()
            .find(|a| a.target == "journeys_adapter")
            .unwrap();
        assert_eq!(
            adapter.executable.as_deref(),
            Some(f.adapter.as_path()),
            "a test binary that is not a product stays as built"
        );
        assert!(raw.broker.as_deref().unwrap().starts_with("carried:"));
    }

    #[test]
    fn a_changed_listing_is_refused() {
        let f = fx("listing");
        let mut text = std::fs::read_to_string(&f.listing).unwrap();
        text.push_str("\n{\"reason\":\"compiler-artifact\",\"package_id\":\"x\",\"target\":{\"name\":\"evil\",\"kind\":[\"bin\"]},\"profile\":{\"test\":false},\"features\":[],\"executable\":\"/evil\",\"fresh\":false}");
        std::fs::write(&f.listing, text).unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::LISTING_MISMATCH,
        );
    }

    #[test]
    fn a_changed_artifact_is_refused_for_a_test_binary_and_for_a_product() {
        let f = fx("artifact");
        std::fs::write(&f.adapter, vec![9u8; 300]).unwrap();
        let m = refused(
            build_from_prebuilt(&f.plan, &f.input),
            code::ARTIFACT_MISMATCH,
        );
        assert!(m.contains("journeys_adapter"), "{m}");
        let f = fx("artifact2");
        let mut bytes = std::fs::read(&f.app).unwrap();
        bytes[0] ^= 0xff;
        std::fs::write(&f.app, bytes).unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            code::ARTIFACT_MISMATCH,
        );
    }

    #[test]
    fn a_signed_copy_changed_after_signing_is_refused() {
        let f = fx("tampered");
        let mut bytes = std::fs::read(&f.signed).unwrap();
        bytes.push(0);
        std::fs::write(&f.signed, bytes).unwrap();
        let m = refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::UNSIGNED_PRODUCT,
        );
        assert!(m.contains("not the evidence's"), "{m}");
    }

    #[test]
    fn an_unsigned_product_is_refused_with_no_evidence_or_no_record_for_it() {
        let mut f = fx("unsigned");
        f.input.evidence = None;
        let m = refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::UNSIGNED_PRODUCT,
        );
        assert!(m.contains("no signing evidence"), "{m}");
        // Evidence for another file (or another build's bytes) does not cover this product.
        let other = Evidence::parse(&evidence_json(
            "debug/other",
            &f.signed,
            &"cd".repeat(32),
            &rightkit_fs::digest::sha256_file(&f.signed).unwrap(),
        ))
        .unwrap();
        f.input.evidence = Some(other);
        let m = refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::UNSIGNED_PRODUCT,
        );
        assert!(m.contains("no signing record names"), "{m}");
    }

    #[test]
    fn a_build_that_is_not_the_declared_one_is_refused() {
        let f = fx("command");
        let mut att: PrebuiltAttestation =
            serde_json::from_slice(&std::fs::read(&f.attestation).unwrap()).unwrap();
        att.command = argv("cargo build -p fxapp --features other --message-format=json");
        std::fs::write(&f.attestation, serde_json::to_vec(&att).unwrap()).unwrap();
        let m = refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::COMMAND_MISMATCH,
        );
        assert!(m.contains("features other"), "{m}");
    }

    #[test]
    fn an_artifact_the_gate_did_not_attest_is_refused() {
        let f = fx("unattested");
        let mut att: PrebuiltAttestation =
            serde_json::from_slice(&std::fs::read(&f.attestation).unwrap()).unwrap();
        att.outputs
            .retain(|o| !o.path.ends_with("journeys_adapter-1234"));
        std::fs::write(&f.attestation, serde_json::to_vec(&att).unwrap()).unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            code::ARTIFACT_NOT_FROM_BUILD,
        );
    }

    #[test]
    fn an_empty_or_malformed_attestation_and_an_empty_listing_are_never_accepted() {
        let f = fx("empty");
        let good: PrebuiltAttestation =
            serde_json::from_slice(&std::fs::read(&f.attestation).unwrap()).unwrap();
        let write = |a: &PrebuiltAttestation| {
            std::fs::write(&f.attestation, serde_json::to_vec(a).unwrap()).unwrap()
        };
        write(&PrebuiltAttestation {
            outputs: vec![],
            ..good.clone()
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::EMPTY_LISTING,
        );
        write(&PrebuiltAttestation {
            schema: "other/1".into(),
            ..good.clone()
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::NOT_ATTESTED,
        );
        write(&PrebuiltAttestation {
            request_id: "not an id".into(),
            ..good.clone()
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::NOT_ATTESTED,
        );
        write(&PrebuiltAttestation {
            listing_sha256: "xyz".into(),
            ..good.clone()
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::NOT_ATTESTED,
        );
        std::fs::write(&f.attestation, b"{\"schema\":\"x\",\"extra\":1}").unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::NOT_ATTESTED,
        );
        // A listing with no executable, whose hash the gate "attested", is an empty build.
        write(&good);
        let text = r#"{"reason":"build-finished","success":true}"#;
        std::fs::write(&f.listing, text).unwrap();
        write(&PrebuiltAttestation {
            listing_sha256: sha256_text(text.as_bytes()),
            ..good.clone()
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::EMPTY_LISTING,
        );
        // A listing whose build did not finish successfully is not attested either.
        let text = "{\"reason\":\"build-finished\",\"success\":false}";
        std::fs::write(&f.listing, text).unwrap();
        write(&PrebuiltAttestation {
            listing_sha256: sha256_text(text.as_bytes()),
            ..good
        });
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            code::BUILD_NOT_ATTESTED,
        );
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn the_two_options_go_together_and_a_missing_file_is_refused() {
        assert!(PrebuiltInput::from_args(None, None).unwrap().is_none());
        assert!(PrebuiltInput::from_args(Some("l".into()), None)
            .unwrap_err()
            .0
            .contains("unverified input"));
        assert!(PrebuiltInput::from_args(None, Some("a".into()))
            .unwrap_err()
            .0
            .contains(LISTING_FLAG));
        let f = fx("missing");
        std::fs::remove_file(&f.attestation).unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::NOT_ATTESTED,
        );
        let f = fx("missing2");
        std::fs::remove_file(&f.listing).unwrap();
        refused(
            build_from_prebuilt(&f.plan, &f.input),
            prebuilt_code::LISTING_MISMATCH,
        );
    }

    #[test]
    fn the_plan_report_names_the_build_to_carry_and_the_files_a_run_will_launch() {
        let f = fx("report");
        let before = plan_report(&f.plan, None).unwrap();
        assert_eq!(before["schema"], BUILD_PLAN_SCHEMA);
        assert_eq!(before["command"][0], "rustright");
        assert_eq!(before["command"][1], "cargo");
        assert_eq!(before["artifacts"][0]["role"], "app");
        assert_eq!(before["artifacts"][0]["marker"], "required");
        assert_eq!(before["artifacts"][1]["marker"], "not_applicable");
        assert!(before["artifacts"][0]["executable"].is_null());
        let after = plan_report(&f.plan, Some(&f.listing)).unwrap();
        assert_eq!(
            after["artifacts"][0]["executable"],
            f.app.to_string_lossy().as_ref()
        );
        assert_eq!(
            after["artifacts"][1]["executable"],
            f.adapter.to_string_lossy().as_ref()
        );
        // A listing that lacks a declared artifact is refused, not reported with a hole.
        std::fs::write(
            &f.listing,
            "{\"reason\":\"build-finished\",\"success\":true}",
        )
        .unwrap();
        assert!(plan_report(&f.plan, Some(&f.listing)).is_err());
    }
}
