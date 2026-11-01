//! QA gate and social packaging for final renders (feature `render-qa`).
//!
//! - [`run::run_qa`] probes a final render with the editing ffmpeg and returns a
//!   typed [`QaReport`] (pass/fail per check, severity, evidence) bound to the
//!   render's BLAKE3.
//! - [`receipt`] writes the report and a content-addressed receipt into the
//!   project (`qa/<render-blake3>.report.json` / `.receipt.json`) and verifies
//!   them again later.
//! - [`package`] builds per-platform packages (YouTube, Shorts, Reels, TikTok)
//!   and refuses any render whose QA report is missing, failed or does not verify.
//!
//! Ported from `video-project`'s `qa.rs`, `qa_probes.rs` and `package.rs`.

pub mod critic;
pub mod error;
pub mod fsutil;
pub mod package;
pub mod probe;
pub mod receipt;
pub mod report;
pub mod retranscribe;
pub mod run;
pub mod spec;

pub use critic::{CriticError, CriticInput, CriticLoop, CriticRunner, Span};
pub use error::QaError;
pub use package::{
    package_render, requalify_package, verify_package, verify_package_report, PackageManifest,
    PackageOutcome, PackageRequest, PackageVerification, Platform, PlatformRules, SocialMetadata,
};
pub use probe::QaTools;
pub use receipt::{qa_gate, verify_receipt, QaOutcome, QaReceipt, VerifiedQa};
pub use report::{
    CheckStatus, MediaFacts, OverallStatus, QaCheck, QaReport, RenderIdentity, Severity,
};
pub use retranscribe::{
    qa_gate_with_retranscription, retranscription_check, RetranscribeError, RetranscribeExpect,
    Retranscriber, Retranscription,
};
pub use run::run_qa;
pub use spec::{parse_srt, CaptionRules, QaCue, QaExpect, QaSpec};
