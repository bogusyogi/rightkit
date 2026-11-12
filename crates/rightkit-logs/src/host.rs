//! Typed host bridges for log and diagnostic commands: an app implements [`LogHost`] /
//! [`DiagnosticHost`] over its own writer and bundle, and the services map a host's `String`
//! failure to a [`HostOperationError`] naming the operation. Moved here from `rightkit-device`.

use std::marker::PhantomData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOperation {
    WriteLog,
    CollectLogs,
    ClearLogs,
    CollectDiagnostics,
    ClearDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostOperationError {
    pub operation: HostOperation,
    pub message: String,
}

impl HostOperationError {
    fn new(operation: HostOperation, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HostOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "host {:?} failed: {}",
            self.operation, self.message
        )
    }
}

impl std::error::Error for HostOperationError {}

/// An app's log sink: write one entry, collect a bundle, clear.
pub trait LogHost<Entry, Bundle> {
    fn write(&self, entry: &Entry) -> Result<(), String>;
    fn collect(&self) -> Result<Bundle, String>;
    fn clear(&self) -> Result<(), String>;
}

pub struct LogService<'a, H, Entry, Bundle> {
    host: &'a H,
    types: PhantomData<fn(Entry) -> Bundle>,
}

impl<'a, H, Entry, Bundle> LogService<'a, H, Entry, Bundle>
where
    H: LogHost<Entry, Bundle>,
{
    pub fn new(host: &'a H) -> Self {
        Self {
            host,
            types: PhantomData,
        }
    }
    pub fn write(&self, entry: &Entry) -> Result<(), HostOperationError> {
        self.host
            .write(entry)
            .map_err(|error| HostOperationError::new(HostOperation::WriteLog, error))
    }
    pub fn collect(&self) -> Result<Bundle, HostOperationError> {
        self.host
            .collect()
            .map_err(|error| HostOperationError::new(HostOperation::CollectLogs, error))
    }
    pub fn clear(&self) -> Result<(), HostOperationError> {
        self.host
            .clear()
            .map_err(|error| HostOperationError::new(HostOperation::ClearLogs, error))
    }
}

/// An app's diagnostics source: collect a bundle, clear.
pub trait DiagnosticHost<Bundle> {
    fn collect(&self) -> Result<Bundle, String>;
    fn clear(&self) -> Result<(), String>;
}

pub struct DiagnosticService<'a, H, Bundle> {
    host: &'a H,
    bundle: PhantomData<fn() -> Bundle>,
}

impl<'a, H, Bundle> DiagnosticService<'a, H, Bundle>
where
    H: DiagnosticHost<Bundle>,
{
    pub fn new(host: &'a H) -> Self {
        Self {
            host,
            bundle: PhantomData,
        }
    }
    pub fn collect(&self) -> Result<Bundle, HostOperationError> {
        self.host
            .collect()
            .map_err(|error| HostOperationError::new(HostOperation::CollectDiagnostics, error))
    }
    pub fn clear(&self) -> Result<(), HostOperationError> {
        self.host
            .clear()
            .map_err(|error| HostOperationError::new(HostOperation::ClearDiagnostics, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host;
    impl LogHost<u8, String> for Host {
        fn write(&self, entry: &u8) -> Result<(), String> {
            if *entry == 0 {
                Err("zero".into())
            } else {
                Ok(())
            }
        }
        fn collect(&self) -> Result<String, String> {
            Ok("bundle".into())
        }
        fn clear(&self) -> Result<(), String> {
            Err("busy".into())
        }
    }

    #[test]
    fn log_service_names_the_failed_operation() {
        let service = LogService::<_, u8, String>::new(&Host);
        assert!(service.write(&1).is_ok());
        let error = service.write(&0).unwrap_err();
        assert_eq!(error.operation, HostOperation::WriteLog);
        assert_eq!(error.message, "zero");
        assert_eq!(service.collect().unwrap(), "bundle");
        assert_eq!(
            service.clear().unwrap_err().operation,
            HostOperation::ClearLogs
        );
    }
}
