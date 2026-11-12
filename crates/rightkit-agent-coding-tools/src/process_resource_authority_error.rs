use rightkit_agent_engine_runtime_resources::AdmissionError;

/// Resource policy cannot silently become optional when a host omits composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAdmissionError {
    Unconfigured,
    Rejected(AdmissionError),
}

impl ProcessAdmissionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unconfigured => "resource_governor_unconfigured",
            Self::Rejected(_) => "resource_admission_denied",
        }
    }
}

impl std::fmt::Display for ProcessAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfigured => write!(
                f,
                "{}: host composition must install ResourceGovernor before process execution",
                self.code(),
            ),
            Self::Rejected(error) => write!(
                f,
                "{}: {:?} requested {} unit(s), limit {}, pressure {:?}",
                self.code(),
                error.kind,
                error.requested,
                error.limit,
                error.pressure,
            ),
        }
    }
}

impl std::error::Error for ProcessAdmissionError {}
