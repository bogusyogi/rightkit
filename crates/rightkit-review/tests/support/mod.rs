//! Shared fixtures for the review tests. A clean 20 s 1080x1920 video that
//! passes the standard rules, and helpers that plant one defect at a time.

#![allow(dead_code)]

use rightkit_review::{
    ArtifactDescription, ArtifactKind, ClaimAnnotation, Declarations, DeclaredWindow,
    DeterminismProbe, Finding, FrameStat, Subject, TextBody, TextRegion, Verdict, WindowKind,
};

pub fn clean_video() -> Subject {
    let frames = (0..=20u64)
        .map(|index| FrameStat {
            time_ms: index * 1000,
            black: false,
            frame_hash: Some(format!("frame-{index}")),
            ambiguous: None,
        })
        .collect();
    Subject {
        artifact: ArtifactDescription {
            artifact_id: "clip.hook-01".to_string(),
            kind: ArtifactKind::Video,
            width: Some(1080),
            height: Some(1920),
            duration_ms: Some(20_000),
            frames: Some(frames),
            max_flashes_per_second: Some(1.0),
            text_regions: Some(vec![TextRegion {
                id: "title".to_string(),
                overflow: Some(false),
                contrast_ratio: Some(7.1),
            }]),
            determinism: Some(DeterminismProbe {
                input_hash: "blake3:input".to_string(),
                output_hashes: vec!["blake3:out".to_string(), "blake3:out".to_string()],
            }),
            licence: Some("cc0".to_string()),
            text: None,
        },
        declarations: Declarations {
            target_width: Some(1080),
            target_height: Some(1920),
            max_duration_ms: Some(60_000),
            allowed_licences: vec!["cc0".to_string()],
            windows: Vec::new(),
        },
    }
}

pub fn frame_mut(subject: &mut Subject, time_ms: u64) -> &mut FrameStat {
    subject
        .artifact
        .frames
        .as_mut()
        .expect("frames present")
        .iter_mut()
        .find(|frame| frame.time_ms == time_ms)
        .expect("frame at time")
}

pub fn declare(subject: &mut Subject, kind: WindowKind, start_ms: u64, end_ms: u64) {
    subject.declarations.windows.push(DeclaredWindow {
        kind,
        start_ms,
        end_ms,
        reason: "brief-declared".to_string(),
    });
}

/// Failed rule ids, sorted, so tests can assert exactly which rules fired.
pub fn failed_ids(findings: &[Finding]) -> Vec<String> {
    let mut ids: Vec<String> = findings
        .iter()
        .filter(|finding| matches!(finding.verdict, Verdict::Failed { .. }))
        .map(|finding| finding.rule_id.clone())
        .collect();
    ids.sort();
    ids
}

pub fn verdict_of<'a>(findings: &'a [Finding], rule_id: &str) -> &'a Verdict {
    &findings
        .iter()
        .find(|finding| finding.rule_id == rule_id)
        .unwrap_or_else(|| panic!("no finding for {rule_id}"))
        .verdict
}

/// A text subject with no claim annotations. Its declarations are empty.
pub fn text_subject(body: &str) -> Subject {
    text_subject_with(body, Vec::new())
}

/// A text subject whose text body carries the given claim annotations.
pub fn text_subject_with(body: &str, claims: Vec<ClaimAnnotation>) -> Subject {
    Subject {
        artifact: ArtifactDescription {
            artifact_id: "post.hook-01".to_string(),
            kind: ArtifactKind::Text,
            width: None,
            height: None,
            duration_ms: None,
            frames: None,
            max_flashes_per_second: None,
            text_regions: None,
            determinism: None,
            licence: None,
            text: Some(TextBody {
                body: body.to_string(),
                claims,
            }),
        },
        declarations: Declarations::default(),
    }
}
