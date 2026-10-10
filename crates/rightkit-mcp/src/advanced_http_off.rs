//! Stand-in for `advanced_http` when the `advanced-http` feature is off: the type is
//! uninhabited, so `Conn::AdvancedHttp` can never be built and its arms are unreachable.
use crate::client::ClientOptions;
use crate::client_features::{CallOptions, ClientExtensions, ElicitationHandler};
use crate::{McpError, Result};
use serde_json::Value;
use std::time::Duration;

pub(crate) enum AdvancedHttp {}

impl AdvancedHttp {
    pub(crate) fn new(
        _server: &str,
        _url: &str,
        _options: &ClientOptions,
        _extensions: &ClientExtensions,
    ) -> Result<Self> {
        Err(McpError::Config(
            "HTTP client extensions need the `advanced-http` feature of rightkit-mcp".into(),
        ))
    }
    pub(crate) fn request(
        &self,
        _method: &str,
        _params: Value,
        _timeout: Duration,
        _options: &CallOptions,
    ) -> Result<Value> {
        match *self {}
    }
    pub(crate) fn notify_with_options(
        &self,
        _method: &str,
        _timeout: Duration,
        _options: &CallOptions,
    ) -> Result<()> {
        match *self {}
    }
    pub(crate) fn set_version(&self, _version: String) {
        match *self {}
    }
    pub(crate) fn mark_started(&self) {
        match *self {}
    }
    pub(crate) fn alive(&self) -> bool {
        match *self {}
    }
    pub(crate) fn session_id(&self) -> Option<String> {
        match *self {}
    }
    pub(crate) fn set_elicitation_handler(&self, _session: &str, _handler: ElicitationHandler) {
        match *self {}
    }
    pub(crate) fn clear_elicitation_handler(&self, _session: &str) {
        match *self {}
    }
}
