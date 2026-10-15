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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::admission::{CancelToken, EffectGate, SettleOutcome};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn app(pid: i32, id: &str) -> AppInfo {
        AppInfo {
            pid,
            app_id: id.into(),
            name: id.into(),
            path: None,
            started: None,
        }
    }

    fn table(pid: i32) -> Option<AppInfo> {
        match pid {
            10 => Some(app(10, "com.test.editor")),
            11 => Some(app(11, "com.test.other")),
            12 => Some(app(12, "com.apple.systempreferences")),
            _ => None,
        }
    }

    fn hook(allow: &[&str]) -> (PolicyAdmission, Arc<Policy>) {
        let p = Arc::new(Policy::new(allow.iter().copied()));
        (PolicyAdmission::new(p.clone(), table), p)
    }

    fn input(target: &str) -> EffectRequest {
        EffectRequest::new(EffectKind::Input, "click", target)
    }

    #[test]
    fn only_listed_resolvable_apps_are_admitted() {
        let (h, _) = hook(&["com.test.editor"]);
        assert_eq!(h.approve(&input("pid:10")), AdmissionDecision::Allow);
        assert!(matches!(
            h.approve(&input("pid:11")),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn an_empty_allowlist_denies_everything() {
        let (h, _) = hook(&[]);
        assert!(matches!(
            h.approve(&input("pid:10")),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn unresolvable_and_non_pid_targets_are_denied() {
        let (h, _) = hook(&["com.test.editor"]);
        for t in ["pid:99", "pid:x", "window:7", "ax-node", "main", ""] {
            assert!(
                matches!(h.approve(&input(t)), AdmissionDecision::Deny { .. }),
                "{t}"
            );
        }
    }

    #[test]
    fn the_built_in_deny_list_beats_the_allowlist() {
        let (h, _) = hook(&["com.apple.systempreferences"]);
        assert!(matches!(
            h.approve(&input("pid:12")),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn the_host_process_is_never_driven() {
        let (h, p) = hook(&["com.test.editor"]);
        p.protect_pid(10);
        assert!(matches!(
            h.approve(&input("pid:10")),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn launches_are_checked_by_executable_name() {
        let (h, _) = hook(&["editor.exe", "com.test.editor"]);
        let launch = |t: &str| EffectRequest::new(EffectKind::Process, "launch", t);
        assert_eq!(
            h.approve(&launch("C:/apps/Editor.exe")),
            AdmissionDecision::Allow
        );
        assert_eq!(
            h.approve(&launch("com.test.editor")),
            AdmissionDecision::Allow
        );
        assert!(matches!(
            h.approve(&launch("C:/apps/other.exe")),
            AdmissionDecision::Deny { .. }
        ));
        assert!(matches!(
            h.approve(&launch("")),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn a_denied_effect_never_runs_its_executor() {
        let (h, _) = hook(&["com.test.editor"]);
        let gate = EffectGate::new(h);
        let ran = AtomicBool::new(false);
        let denied = gate.admit(input("pid:11"), &CancelToken::new(), |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(denied.settlement.outcome, SettleOutcome::Denied);
        assert!(!denied.settlement.executed);
        assert!(!ran.load(Ordering::SeqCst));
        let allowed = gate.admit(input("pid:10"), &CancelToken::new(), |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(allowed.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }
}
