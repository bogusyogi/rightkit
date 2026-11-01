#![forbid(unsafe_code)]

pub mod budget;
pub mod command;
pub mod digest;
pub mod digest_protocol;
pub mod error;
pub mod evidence;
pub mod host;
pub mod receipt;
pub mod reddit_protocol;
pub mod reddit_reader;
pub mod report;
pub mod research_port;
pub mod source;
pub mod wf_port;
pub mod workflow;

pub use budget::{BudgetAccount, BudgetLimits, BudgetSnapshot, BudgetUsage};
pub use error::ResearchError;
pub use evidence::{Claim, EvidenceKind, EvidenceLedger, EvidenceRecord};
pub use receipt::ResearchReceipt;
pub use report::{ReportClaim, ReportStatus, ResearchReport};
pub use source::{
    registrable_domain, summarize_independence, IndependenceBasis, IndependenceKey,
    IndependenceSummary, InjectedSource, SourceClient, SourceHit, SourceKind, SourceRecord,
};
pub use workflow::{
    Cancellation, NullableString, ResearchAuthorization, ResearchNumber, ResearchPatient,
    ResearchRoute, ResearchSubject, ResearchValue, ResearchWorkflow, SourceFailure, StageRecord,
    WorkflowOutcome, WorkflowRequest, WorkflowStage, WorkflowStatus,
};
