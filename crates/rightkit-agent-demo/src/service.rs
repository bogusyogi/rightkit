//! The `rightkit.agent.demo` service: the host side of the wire contract in `rightkit-agent-tools`.
//!
//! Only the six `demo.*` methods exist, and only for the `right-ctl` app label. The wire types, error
//! codes and service name come from the contract crate, so the host and the client cannot drift. The
//! handler does no policy of its own: it parses, calls the host, and maps `DemoError` to the wire.

use std::sync::Weak;

use rightkit_agent_tools::backends::demo::{
    ActParams, BatchParams, DemoErrorCode, Method, RequestParams, SessionToken,
};
use rightkit_daemon::{CallContext, Emit, Handler, ServiceError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::driver::DemoError;
use super::host::DemoHost;

pub struct DemoService {
    host: Weak<DemoHost>,
}

impl DemoService {
    pub fn new(host: Weak<DemoHost>) -> Self {
        Self { host }
    }
}

#[derive(Deserialize)]
struct TokenParams {
    token: SessionToken,
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, DemoError> {
    serde_json::from_value(params.clone()).map_err(|e| {
        DemoError::of(
            DemoErrorCode::InvalidArguments,
            format!("arguments do not match the contract: {e}"),
        )
    })
}

fn reply<T: Serialize>(v: T) -> Result<Value, DemoError> {
    serde_json::to_value(v).map_err(|e| DemoError::raw("internal", e.to_string()))
}

fn method(name: &str) -> Option<Method> {
    rightkit_agent_tools::backends::demo::METHODS
        .into_iter()
        .find(|m| m.name() == name)
}

impl Handler for DemoService {
    fn call(
        &self,
        cx: &CallContext,
        name: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        let Some(host) = self.host.upgrade() else {
            return Err(DemoError::of(
                DemoErrorCode::ServiceUnavailable,
                format!("{} is shutting down", crate::identity().product_name),
            )
            .into());
        };
        let Some(method) = method(name) else {
            return Err(ServiceError::new(
                "unknown_method",
                format!("{name} is not a demo method"),
            ));
        };
        let run = || -> Result<Value, DemoError> {
            match method {
                Method::Apps => reply(host.apps()?),
                Method::Request => reply(host.request(cx, parse::<RequestParams>(params)?)?),
                Method::Snapshot => {
                    let p: TokenParams = parse(params)?;
                    reply(host.snapshot(&p.token)?)
                }
                Method::Act => {
                    let p: ActParams = parse(params)?;
                    reply(host.act(&p.token, &p.step)?)
                }
                Method::Batch => {
                    let p: BatchParams = parse(params)?;
                    reply(host.batch(&p.token, &p.steps)?)
                }
                Method::End => {
                    let p: TokenParams = parse(params)?;
                    reply(host.end(&p.token)?)
                }
            }
        };
        run().map_err(ServiceError::from)
    }
}
