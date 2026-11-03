//! The allowlist as an admission hook, so effects admitted through an
//! [`EffectGate`](super::admission::EffectGate) obey the same policy as
//! [`crate::Automation`]: deny by default, the built-in deny list, host-process
//! protection and the kill switch (all in [`Policy::check`]).
//!
//! A request is approved only when its target resolves to an app the policy allows:
//!
//! - `Input`, `Action`, `Capture`, `Script`, `Navigate`, `Command`: the target must be
//!   `pid:<n>` and `resolve(n)` must name the app. Any other target (`window:7`,
//!   `ax-node`, `main`) cannot be tied to an app, so it is denied.
//! - `Process` (launch): the target is an app id or an executable path; the file name
//!   of a path is checked against the allowlist (lowercase exe names on Windows,
//!   bundle ids on macOS), so a path to a non-listed executable is denied.

use super::admission::{AdmissionDecision, AdmissionHook, EffectKind, EffectRequest};
use crate::model::AppInfo;
use crate::policy::Policy;
use std::sync::Arc;

/// Looks a pid up in the live process table. `None` means "unknown", which is denied.
pub type ResolveApp = Box<dyn Fn(i32) -> Option<AppInfo> + Send + Sync>;

/// Admission hook that applies a [`Policy`] to every effect.
pub struct PolicyAdmission {
    policy: Arc<Policy>,
    resolve: ResolveApp,
}

impl PolicyAdmission {
    pub fn new(
        policy: Arc<Policy>,
        resolve: impl Fn(i32) -> Option<AppInfo> + Send + Sync + 'static,
    ) -> Self {
        Self {
            policy,
            resolve: Box::new(resolve),
        }
    }

    pub fn policy(&self) -> &Arc<Policy> {
        &self.policy
    }

    fn app_for(&self, req: &EffectRequest) -> Result<AppInfo, String> {
        if req.kind == EffectKind::Process {
            let id = std::path::Path::new(&req.target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .ok_or_else(|| "launch target names no app".to_string())?;
            return Ok(AppInfo {
                pid: 0,
                app_id: id.clone(),
                name: id,
                path: Some(req.target.clone()),
                started: None,
            });
        }
        let pid: i32 = req
            .target
            .strip_prefix("pid:")
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| {
                format!(
                    "target {:?} is not addressed to an app (deny by default)",
                    req.target
                )
            })?;
        (self.resolve)(pid).ok_or_else(|| format!("pid {pid} does not resolve to an app"))
    }
}

impl AdmissionHook for PolicyAdmission {
    fn approve(&self, req: &EffectRequest) -> AdmissionDecision {
        let app = match self.app_for(req) {
            Ok(a) => a,
            Err(reason) => return AdmissionDecision::deny(reason),
        };
        match self.policy.check(&app) {
            Ok(()) => AdmissionDecision::Allow,
            Err(e) => AdmissionDecision::deny(e.to_string()),
        }
    }
}
