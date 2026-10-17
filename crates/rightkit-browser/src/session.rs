use crate::config::{
    reject_real_browser_profile, resolve_executable, validate_extra_args, validate_profile_dir,
    validate_profile_name, LaunchOptions, ProfileSpec,
};
use crate::error::{cdp, BrowserError, Result};
use crate::fixture_protocol::FixtureError;
use crate::page::BrowserPage;
#[cfg(target_os = "macos")]
use crate::policy::NetworkPolicy;
use crate::policy::{Guard, LifecycleKind};
use crate::proc::ChromeProc;
use crate::proxy::{FixtureProxyPolicy, PinProxy, PinnedConnection};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    PermissionDescriptor, PermissionSetting, ResetPermissionsParams, SetDownloadBehaviorBehavior,
    SetDownloadBehaviorParams, SetPermissionParams,
};
use chromiumoxide::handler::HandlerConfig;
use futures::StreamExt;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Per-step wait before escalating a close (CDP -> SIGTERM -> SIGKILL).
const GRACE: Duration = Duration::from_secs(5);
const FIXTURE_STOP_RECEIPT_GRACE: Duration = Duration::from_secs(12);

#[cfg(windows)]
#[derive(Clone, Copy)]
enum FixtureBackend {
    WinAppcontainer,
}

enum ProfileGuard {
    Temp(#[allow(dead_code)] tempfile::TempDir),
    Named {
        _lock: std::fs::File,
    },
    /// Caller-owned directory: dropping this guard has no filesystem effects.
    Existing,
}

struct FixtureLaunchState {
    process: Arc<StdMutex<Option<ChromeProc>>>,
    profile: StdMutex<Option<ProfileGuard>>,
    proxy: StdMutex<Option<PinProxy>>,
    spawn_complete: tokio::sync::watch::Sender<u8>,
    armed: AtomicBool,
    cleanup_started: AtomicBool,
}

struct FixtureLaunchOwner {
    state: Arc<FixtureLaunchState>,
}

struct SpawnCompleteGuard {
    state: Option<Arc<FixtureLaunchState>>,
    result: u8,
}

struct FixtureStopCompletionGuard {
    status: tokio::sync::watch::Sender<u8>,
    complete: bool,
}

impl Drop for SpawnCompleteGuard {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.spawn_complete.send_replace(self.result);
        }
    }
}

impl FixtureStopCompletionGuard {
    fn stopped(&mut self) {
        self.status.send_replace(2);
        self.complete = true;
    }
}

impl Drop for FixtureStopCompletionGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.status.send_replace(3);
        }
    }
}

impl FixtureLaunchOwner {
    fn new(profile: ProfileGuard, proxy: Option<PinProxy>) -> Self {
        let (spawn_complete, _) = tokio::sync::watch::channel(0u8);
        Self {
            state: Arc::new(FixtureLaunchState {
                process: Arc::new(StdMutex::new(None)),
                profile: StdMutex::new(Some(profile)),
                proxy: StdMutex::new(proxy),
                spawn_complete,
                armed: AtomicBool::new(true),
                cleanup_started: AtomicBool::new(false),
            }),
        }
    }

    fn process_slot(&self) -> Arc<StdMutex<Option<ChromeProc>>> {
        self.state.process.clone()
    }

    fn spawn_state(&self) -> Arc<FixtureLaunchState> {
        self.state.clone()
    }

    fn into_parts(
        self,
    ) -> (
        Arc<StdMutex<Option<ChromeProc>>>,
        ProfileGuard,
        Option<PinProxy>,
    ) {
        self.state.armed.store(false, Ordering::SeqCst);
        let profile = self.state.profile.lock().unwrap().take().unwrap();
        let proxy = self.state.proxy.lock().unwrap().take();
        (self.state.process.clone(), profile, proxy)
    }
}

impl FixtureLaunchState {
    async fn cleanup(self: Arc<Self>) {
        let mut complete = self.spawn_complete.subscribe();
        while *complete.borrow() == 0 {
            if complete.changed().await.is_err() {
                self.keep_profile();
                return;
            }
        }

        let process = self
            .process
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let spawn_status = *complete.borrow();
        let process_stopped = match (spawn_status, process) {
            (_, Some(mut process)) => {
                process.begin_fixture_transport_shutdown();
                let terminated = tokio::task::spawn_blocking(move || {
                    let result = process.terminate_fixture_tree();
                    (process, result)
                })
                .await;
                let Ok((process, result)) = terminated else {
                    return self.keep_profile();
                };
                if result.is_err() {
                    *self
                        .process
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(process);
                    false
                } else {
                    #[cfg(windows)]
                    {
                        let mut process = process;
                        if process
                            .finish_fixture_transport_shutdown(Instant::now() + GRACE)
                            .await
                            .is_err()
                        {
                            *self
                                .process
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(process);
                            false
                        } else {
                            true
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        true
                    }
                }
            }
            (2, None) => true,
            (_, None) => false,
        };
        if !process_stopped {
            self.keep_profile();
            return;
        }

        let proxy = self
            .proxy
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let proxy_stopped = match proxy {
            Some(proxy) => proxy.shutdown(GRACE).await.is_ok(),
            None => true,
        };
        if proxy_stopped {
            release_profile(
                self.profile
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take(),
            );
        } else {
            self.keep_profile();
        }
    }

    fn keep_profile(&self) {
        if let Some(ProfileGuard::Temp(profile)) = self
            .profile
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = profile.keep();
        }
    }
}

impl Drop for FixtureLaunchOwner {
    fn drop(&mut self) {
        if !self.state.armed.load(Ordering::SeqCst)
            || self.state.cleanup_started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let state = self.state.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(state.cleanup());
        } else {
            if *self.state.spawn_complete.subscribe().borrow() == 0 {
                self.state.keep_profile();
                return;
            }
            let process = self
                .state
                .process
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            let spawn_status = *self.state.spawn_complete.subscribe().borrow();
            let process_stopped = match (spawn_status, process) {
                (_, Some(mut process)) => process.terminate_fixture_tree().is_ok(),
                (2, None) => true,
                (_, None) => false,
            };
            if process_stopped {
                #[cfg(windows)]
                {
                    // No async runtime means bridge finalization cannot obtain its
                    // inertness receipt. Keep profile evidence instead of claiming
                    // an unverified cleanup path.
                    self.state.keep_profile();
                }
                #[cfg(not(windows))]
                {
                    self.state
                        .proxy
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    release_profile(
                        self.state
                            .profile
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take(),
                    );
                }
            } else {
                self.state.keep_profile();
            }
        }
    }
}

/// Remove the session's profile. A temporary profile is deleted with bounded retries:
/// on Windows a just-terminated helper can hold a handle for a moment after its job ends.
fn release_profile(guard: Option<ProfileGuard>) {
    if let Some(ProfileGuard::Temp(dir)) = guard {
        let path = dir.path().to_path_buf();
        drop(dir);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while path.exists()
            && std::fs::remove_dir_all(&path).is_err()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn lock_named_profile(root: &Path, name: &str) -> Result<(ProfileGuard, PathBuf, PathBuf)> {
    validate_profile_name(name)?;
    reject_real_browser_profile(root)?;
    let dir = root.join(name);
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(".rightkit-browser.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(BrowserError::ProfileInUse(name.to_string()));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    let dl = dir.join("rightkit-downloads");
    Ok((ProfileGuard::Named { _lock: lock }, dir, dl))
}

fn prepare_profile(profile: &ProfileSpec) -> Result<(ProfileGuard, PathBuf, PathBuf)> {
    match profile {
        ProfileSpec::Temporary => {
            let t = tempfile::tempdir()?;
            let p = t.path().to_path_buf();
            let dl = p.join("downloads");
            Ok((ProfileGuard::Temp(t), p, dl))
        }
        ProfileSpec::Named { root, name } => lock_named_profile(root, name),
        ProfileSpec::ImportedFrom { root, name, import } => {
            // Consent is checked before any directory is created or browser data is read.
            import.validate()?;
            let locked = lock_named_profile(root, name)?;
            crate::real_session::import_if_needed(import, &locked.1)?;
            Ok(locked)
        }
        ProfileSpec::Directory(path) => {
            validate_profile_dir(path)?;
            Ok((
                ProfileGuard::Existing,
                path.clone(),
                path.join("rightkit-downloads"),
            ))
        }
    }
}

/// One Chrome process with any number of pages.
pub struct BrowserSession {
    id: String,
    browser: Mutex<Browser>,
    pages: StdMutex<Vec<Arc<BrowserPage>>>,
    current: StdMutex<Option<String>>,
    page_seq: AtomicU64,
    handler: StdMutex<Option<JoinHandle<()>>>,
    watcher: StdMutex<Option<JoinHandle<()>>>,
    /// Released (temporary profile deleted) once Chrome has exited, on shutdown or drop.
    profile_guard: StdMutex<Option<ProfileGuard>>,
    profile_dir: PathBuf,
    fixture_profile_path: Option<PathBuf>,
    download_dir: PathBuf,
    upload_root: Option<PathBuf>,
    headless: bool,
    guard: Arc<Guard>,
    pid: Option<u32>,
    proc: Arc<StdMutex<Option<ChromeProc>>>,
    fixture_stop_status: tokio::sync::watch::Sender<u8>,
    fixture_stop_failure: Arc<StdMutex<Option<String>>>,
    fixture_stop_lock: Mutex<()>,
    proxy: Option<PinProxy>,
    /// Set before an intentional stop so the CDP pump does not report a crash.
    stopping: Arc<AtomicBool>,
    stopped: AtomicBool,
    fixture_mode: bool,
    #[cfg(windows)]
    fixture_backend: Option<FixtureBackend>,
}

/// Flags every session launch starts from, before profile, viewport, proxy and caller flags.
const BASE_CHROME_ARGS: &[&str] = &[
    "--remote-debugging-port=0",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    // Chrome's own traffic: the component updater (clients2.google.com) still runs
    // with background networking off, and a session must see only its pages' requests.
    "--disable-component-update",
    "--disable-domain-reliability",
    "--no-pings",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-breakpad",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--disable-default-apps",
    "--disable-dev-shm-usage",
    "--disable-extensions",
    "--disable-hang-monitor",
    "--disable-ipc-flooding-protection",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--disable-sync",
    "--force-color-profile=srgb",
    "--metrics-recording-only",
    "--password-store=basic",
    "--use-mock-keychain",
    // No unmanaged windows: a pop-up would be a page the policy never saw.
    "--block-new-web-contents",
];

impl BrowserSession {
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        Self::launch_inner(opts, None).await
    }

    /// Start a visible session that reuses the user's own browser sign-ins, with consent.
    ///
    /// Chrome 136+ ignores a debugging port on the browser's default user-data directory and a
    /// profile in use is locked, so the user's running browser is not driven in place: the
    /// consented profile's sign-in files are copied into a private RightKit profile under `root`
    /// and the same browser (same executable, so its sign-in key applies) is launched on that
    /// copy over a debugging pipe. The consented domains confine every action, passwords are
    /// refused, and `consent` is checked at launch and before each action.
    ///
    /// Chromium-family browsers only (Chrome, Edge, Brave, Chromium); Safari and Firefox return
    /// [`BrowserError::Unsupported`]. `opts` supplies timeouts, network policy and the admission
    /// hook (which runs after the consent check); its profile, executable and window mode are
    /// replaced.
    pub async fn attach_user_browser(
        consent: crate::real_session::UserConsentReceipt,
        root: impl Into<PathBuf>,
        opts: LaunchOptions,
    ) -> Result<Self> {
        Self::attach_user_browser_revocable(
            consent,
            crate::real_session::ConsentRevocation::new(),
            root,
            opts,
        )
        .await
    }

    /// [`Self::attach_user_browser`] with a live [`crate::ConsentRevocation`]: revoking it
    /// makes the session's next action fail.
    pub async fn attach_user_browser_revocable(
        consent: crate::real_session::UserConsentReceipt,
        revocation: crate::real_session::ConsentRevocation,
        root: impl Into<PathBuf>,
        mut opts: LaunchOptions,
    ) -> Result<Self> {
        use crate::real_session::{
            attach_profile_name, browser_executable, BrowserKind, BrowserSource, ImportSpec,
        };
        consent.check_active()?;
        let source = match consent.browser {
            BrowserKind::Chrome => BrowserSource::Chrome,
            BrowserKind::Edge => BrowserSource::Edge,
            BrowserKind::Safari | BrowserKind::Firefox => {
                return Err(BrowserError::Unsupported(format!(
                    "{} cannot be attached over CDP; drive it through rightkit-automation",
                    consent.browser.name()
                )));
            }
            other => {
                return Err(BrowserError::Unsupported(format!(
                    "attach supports Chrome and Edge; {} is not covered yet",
                    other.name()
                )));
            }
        };
        let exe = browser_executable(consent.browser).ok_or(BrowserError::ChromeNotFound)?;
        let name = attach_profile_name(&consent);
        let import = ImportSpec {
            browser: source,
            source_profile: Some(consent.profile.clone()),
            domains: consent.domains.clone(),
            consent,
            revocation,
            reimport: false,
        };
        opts.chrome_path = Some(exe);
        opts.use_system_chrome_only = false;
        opts.allow_system_chrome = false;
        opts.headless = false;
        opts.profile = ProfileSpec::ImportedFrom {
            root: root.into(),
            name,
            import,
        };
        Self::launch(opts).await
    }

    /// Launch the sealed source-only fixture browser. This private constructor fixes every
    /// option that could widen authority and installs the fixture-only network policy.
    pub(crate) async fn launch_fixture(
        policy: FixtureProxyPolicy,
    ) -> std::result::Result<Self, FixtureError> {
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = policy;
            return Err(FixtureError::ConfinementUnsupported(
                "source-only browser fixtures require macOS Seatbelt confinement".into(),
            ));
        }
        #[cfg(target_os = "macos")]
        {
            use crate::policy::{AddressClass, PolicyDecision};
            let permitted_urls = policy.permitted_urls.clone();
            let remaining = policy.expires_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FixtureError::Revoked(
                    "fixture lease expired before browser launch".into(),
                ));
            }
            let opts = LaunchOptions {
                headless: true,
                mute_audio: true,
                profile: ProfileSpec::Temporary,
                chrome_path: None,
                allow_system_chrome: false,
                use_system_chrome_only: false,
                extra_args: Vec::new(),
                download_dir: None,
                upload_root: None,
                network: NetworkPolicy::default()
                    .allow_class(AddressClass::Loopback)
                    .allow_with(move |request| {
                        if permitted_urls.contains(&request.url) {
                            PolicyDecision::Allow
                        } else {
                            PolicyDecision::Deny(
                                "fixture URL is outside the immutable bundle".into(),
                            )
                        }
                    }),
                launch_timeout: remaining,
                request_timeout: remaining.min(Duration::from_secs(5)),
                ..Default::default()
            };
            Self::launch_inner(opts, Some(policy))
                .await
                .map_err(|error| FixtureError::ConfinementUnavailable(error.to_string()))
        }
        #[cfg(windows)]
        {
            let remaining = policy.expires_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FixtureError::Revoked(
                    "fixture lease expired before browser launch".into(),
                ));
            }
            // AppContainer denies all network. CDP uses inherited pipes, while
            // Fetch fulfills immutable fixture bytes without a TCP proxy.
            let opts = LaunchOptions {
                headless: true,
                mute_audio: true,
                profile: ProfileSpec::Temporary,
                chrome_path: None,
                allow_system_chrome: false,
                use_system_chrome_only: false,
                extra_args: Vec::new(),
                download_dir: None,
                upload_root: None,
                network: crate::policy::NetworkPolicy::unrestricted(),
                launch_timeout: remaining,
                request_timeout: remaining.min(Duration::from_secs(5)),
                ..Default::default()
            };
            Self::launch_inner(opts, Some(policy))
                .await
                .map_err(|error| FixtureError::ConfinementUnavailable(error.to_string()))
        }
    }

    async fn launch_inner(
        #[cfg_attr(not(windows), allow(unused_mut))] mut opts: LaunchOptions,
        fixture_proxy_policy: Option<FixtureProxyPolicy>,
    ) -> Result<Self> {
        validate_extra_args(&opts.extra_args)?;
        let exe = resolve_executable(&opts)?;
        // A sandbox that cannot read its own browser crashes the network service at startup;
        // refuse that launch with the directory and the fix instead of a browser that hangs.
        let sandboxed = !opts
            .extra_args
            .iter()
            .any(|arg| arg.trim_start_matches('-') == "no-sandbox");
        if sandboxed {
            if let Some(missing) = crate::sandbox_access::missing_access(&exe) {
                return Err(BrowserError::ChromeSandboxAccess {
                    fix: crate::sandbox_access::grant_command(&missing.directory),
                    directory: missing.directory,
                    principals: missing.principals,
                });
            }
        }
        let (profile, profile_dir, default_dl) = prepare_profile(&opts.profile)?;
        // A consented real-session profile confines every action to the consented domains.
        let admission = match &opts.profile {
            ProfileSpec::ImportedFrom { import, .. } => Some(
                crate::real_session::scoped_admission(import, opts.admission.clone())?,
            ),
            _ => opts.admission.clone(),
        };
        let mut profile_guard = Some(profile);
        let download_dir = opts.download_dir.clone().unwrap_or(default_dl);
        std::fs::create_dir_all(&download_dir)?;

        let id = format!("bs-{}", SESSION_SEQ.fetch_add(1, Ordering::Relaxed));
        let guard = Arc::new(Guard {
            session_id: id.clone(),
            admission,
            network: opts.network.clone(),
            events: opts.on_event.clone(),
        });
        let fixture_mode = fixture_proxy_policy.is_some();
        #[cfg(windows)]
        let fixture_backend = fixture_mode.then_some(FixtureBackend::WinAppcontainer);
        #[cfg(windows)]
        let windows_fixture = matches!(fixture_backend, Some(FixtureBackend::WinAppcontainer));
        #[cfg(not(windows))]
        let windows_fixture = false;
        let mut proxy = if let Some(policy) = fixture_proxy_policy {
            if windows_fixture {
                None
            } else {
                Some(
                    PinProxy::start_fixture(guard.clone(), policy)
                        .await
                        .map_err(|error| {
                            BrowserError::Launch(format!("fixture proxy unavailable: {error}"))
                        })?,
                )
            }
        } else if opts.network.unrestricted {
            None
        } else {
            Some(PinProxy::start(guard.clone()).await?)
        };

        let mut args: Vec<OsString> = BASE_CHROME_ARGS.iter().map(OsString::from).collect();
        args.push(crate::proc::profile_arg(&profile_dir));
        args.push(format!("--window-size={},{}", opts.viewport.0, opts.viewport.1).into());
        args.push(if opts.headless {
            "--headless=new".into()
        } else {
            "--hide-crash-restore-bubble".into()
        });
        if opts.mute_audio {
            args.push("--mute-audio".into());
        }
        if let Some(p) = &proxy {
            // Every connection, loopback included, goes through the pinning proxy;
            // Chrome itself resolves nothing but the proxy address.
            args.push(format!("--proxy-server=http://{}", p.addr).into());
            args.push("--proxy-bypass-list=<-loopback>".into());
            args.push("--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".into());
            // Keep cross-site frames in the page process so interception covers them.
            args.push("--disable-site-isolation-trials".into());
            // No UDP path around the proxy: WebRTC only over proxied routes, no QUIC.
            args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
            args.push("--disable-quic".into());
        }
        for a in &opts.extra_args {
            args.push(format!("--{}", a.trim_start_matches('-')).into());
        }
        args.push("about:blank".into());
        let launch_timeout = opts.launch_timeout;
        let request_timeout = opts.request_timeout;
        let fixture_profile_dir = fixture_mode.then(|| profile_dir.clone());
        let fixture_proxy_addr = fixture_mode
            .then(|| proxy.as_ref().map(|proxy| proxy.addr))
            .flatten();
        let fixture_owner = if fixture_mode {
            Some(FixtureLaunchOwner::new(
                profile_guard.take().expect("fixture profile is owned"),
                proxy.take(),
            ))
        } else {
            None
        };
        let proc = fixture_owner
            .as_ref()
            .map(FixtureLaunchOwner::process_slot)
            .unwrap_or_else(|| Arc::new(StdMutex::new(None)));
        let spawn_owner = fixture_owner.as_ref().map(FixtureLaunchOwner::spawn_state);
        #[cfg(windows)]
        let job = opts.windows_job.take();
        #[cfg(not(windows))]
        let job = None;
        let spawn_proc_slot = proc.clone();
        let spawn_proc = tokio::task::spawn_blocking(move || {
            let mut completion = SpawnCompleteGuard {
                state: spawn_owner.clone(),
                result: 3,
            };
            let spawned = if fixture_profile_dir.is_some() && windows_fixture {
                #[cfg(windows)]
                {
                    args.retain(|arg| {
                        !arg.to_string_lossy()
                            .starts_with("--remote-debugging-port=")
                    });
                    args.push("--remote-debugging-pipe".into());
                    // Chrome's browser process always starts the crashpad handler, which cannot
                    // create its pipe in an AppContainer; the fixture runs the headless shell.
                    crate::config::chrome_headless_shell_path(&exe).and_then(|shell| {
                        crate::proc::spawn_chrome_fixture_windows(
                            &shell,
                            &args,
                            fixture_profile_dir.as_deref().expect("fixture profile"),
                        )
                    })
                }
                #[cfg(not(windows))]
                unreachable!("windows fixture is cfg-gated")
            } else if let (Some(profile), Some(proxy_addr)) =
                (fixture_profile_dir.as_deref(), fixture_proxy_addr)
            {
                #[cfg(target_os = "macos")]
                {
                    crate::proc::spawn_chrome_fixture(
                        &exe,
                        &args,
                        launch_timeout,
                        profile,
                        proxy_addr,
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (profile, proxy_addr);
                    Err(BrowserError::Launch(
                        "fixture confinement is unsupported on this platform".into(),
                    ))
                }
            } else {
                crate::proc::spawn_chrome(&exe, &args, launch_timeout, job)
            };
            match spawned {
                Ok(process) => {
                    let info = (process.id(), process.ws_url.clone());
                    *spawn_proc_slot
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(process);
                    completion.result = 1;
                    Ok(info)
                }
                Err(error) => {
                    completion.result = 2;
                    Err(error)
                }
            }
        })
        .await;
        let spawn_info = match spawn_proc {
            Ok(Ok(info)) => info,
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                if let Some(owner) = &fixture_owner {
                    owner.state.spawn_complete.send_replace(3);
                }
                return Err(BrowserError::Launch(error.to_string()));
            }
        };
        let pid = Some(spawn_info.0);
        #[cfg(windows)]
        let connect_url = if windows_fixture {
            let deadline = Instant::now() + launch_timeout;
            let mut process = proc.lock().unwrap_or_else(|error| error.into_inner());
            process
                .as_mut()
                .ok_or_else(|| {
                    BrowserError::Launch("Windows fixture process ownership is absent".into())
                })?
                .start_fixture_pipe_bridge(deadline)?
        } else {
            spawn_info.1
        };
        #[cfg(not(windows))]
        let connect_url = spawn_info.1;
        let config = HandlerConfig {
            request_timeout,
            ..HandlerConfig::default()
        };
        let (browser, mut handler) = Browser::connect_with_config(connect_url, config)
            .await
            .map_err(|e| {
                #[cfg(windows)]
                if windows_fixture {
                    let diagnostics = proc
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .as_ref()
                        .map(ChromeProc::fixture_diagnostics)
                        .unwrap_or_else(|| {
                            "windows fixture diagnostic process ownership absent".into()
                        });
                    return BrowserError::Launch(format!(
                        "Windows fixture CDP connection failed: {e}; {diagnostics}"
                    ));
                }
                BrowserError::Launch(e.to_string())
            })?;
        let (fixture_stop_status, _) = tokio::sync::watch::channel(0u8);
        let stopping = Arc::new(AtomicBool::new(false));
        let crashed = Arc::new(AtomicBool::new(false));
        let (pump_proc, pump_stopping, pump_guard, pump_crashed) = (
            proc.clone(),
            stopping.clone(),
            guard.clone(),
            crashed.clone(),
        );
        let pump_fixture_mode = fixture_mode;
        let pump = tokio::spawn(async move {
            // Transient handler errors are tolerated while Chrome is alive; the
            // stream ending (or Chrome exiting) ends the session.
            let mut errors = 0u32;
            while let Some(ev) = handler.next().await {
                if ev.is_err() {
                    errors += 1;
                    if errors > 50
                        || if pump_fixture_mode {
                            chrome_has_exited(&pump_proc)
                        } else {
                            chrome_exit(&pump_proc).is_some()
                        }
                    {
                        break;
                    }
                } else {
                    errors = 0;
                }
            }
            // Give the OS a moment to reap so the reason carries the exit status.
            let mut status = None;
            for _ in 0..25 {
                status = if pump_fixture_mode {
                    None
                } else {
                    chrome_exit(&pump_proc)
                };
                if (pump_fixture_mode && chrome_has_exited(&pump_proc))
                    || status.is_some()
                    || pump_stopping.load(Ordering::SeqCst)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            report_crash(&pump_guard, &pump_stopping, &pump_crashed, status, pid);
        });
        // The CDP stream does not always end when Chrome dies, so also watch the process.
        let (w_proc, w_stopping, w_crashed, w_guard) = (
            proc.clone(),
            stopping.clone(),
            crashed.clone(),
            guard.clone(),
        );
        let watcher_fixture_mode = fixture_mode;
        let watcher = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if w_stopping.load(Ordering::SeqCst) || w_crashed.load(Ordering::SeqCst) {
                    return;
                }
                if watcher_fixture_mode && chrome_has_exited(&w_proc) {
                    report_crash(&w_guard, &w_stopping, &w_crashed, None, pid);
                    return;
                }
                if let Some(status) = (!watcher_fixture_mode)
                    .then(|| chrome_exit(&w_proc))
                    .flatten()
                {
                    report_crash(&w_guard, &w_stopping, &w_crashed, Some(status), pid);
                    return;
                }
            }
        });
        let (proc, profile_guard, proxy) = match fixture_owner {
            Some(owner) => {
                let (proc, profile, proxy) = owner.into_parts();
                (proc, Some(profile), proxy)
            }
            None => (proc, profile_guard.take(), proxy),
        };
        let fixture_profile_path = fixture_mode.then(|| profile_dir.clone());
        let session = Self {
            id,
            browser: Mutex::new(browser),
            pages: StdMutex::new(Vec::new()),
            current: StdMutex::new(None),
            page_seq: AtomicU64::new(0),
            handler: StdMutex::new(Some(pump)),
            watcher: StdMutex::new(Some(watcher)),
            profile_guard: StdMutex::new(profile_guard),
            profile_dir,
            fixture_profile_path,
            download_dir,
            upload_root: opts.upload_root,
            headless: opts.headless,
            guard,
            pid,
            proc,
            fixture_stop_status,
            fixture_stop_failure: Arc::new(StdMutex::new(None)),
            fixture_stop_lock: Mutex::new(()),
            proxy,
            stopping,
            stopped: AtomicBool::new(false),
            fixture_mode,
            #[cfg(windows)]
            fixture_backend,
        };
        if !fixture_mode {
            let download_setup = session
                .browser
                .lock()
                .await
                .execute(
                    SetDownloadBehaviorParams::builder()
                        .behavior(SetDownloadBehaviorBehavior::Allow)
                        .download_path(session.download_dir.to_string_lossy().to_string())
                        .build()
                        .map_err(BrowserError::Launch)?,
                )
                .await
                .map_err(cdp);
            if let Err(error) = download_setup {
                session.stop(false).await;
                return Err(BrowserError::Launch(error.to_string()));
            }
        }

        let mode = if opts.headless { "headless" } else { "headed" };
        session
            .guard
            .emit_kind(LifecycleKind::Start, format!("launched ({mode})"), |e| {
                e.pid = pid
            });
        Ok(session)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) async fn configure_fixture_downloads(
        &self,
    ) -> std::result::Result<(), FixtureError> {
        let configured = self
            .browser
            .lock()
            .await
            .execute(
                SetDownloadBehaviorParams::builder()
                    .behavior(SetDownloadBehaviorBehavior::Deny)
                    .download_path(self.download_dir.to_string_lossy().to_string())
                    .build()
                    .map_err(|error| FixtureError::Browser(error.to_string()))?,
            )
            .await
            .map_err(|error| FixtureError::Browser(error.to_string()));
        #[cfg(windows)]
        if let Err(error) = configured {
            return Err(FixtureError::Browser(format!(
                "{error}; {}",
                self.fixture_cdp_diagnostics()
            )));
        }
        #[cfg(not(windows))]
        configured?;
        Ok(())
    }

    #[cfg(windows)]
    fn fixture_cdp_diagnostics(&self) -> String {
        self.proc
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(ChromeProc::fixture_diagnostics)
            .unwrap_or_else(|| "windows fixture diagnostic process ownership absent".into())
    }
    /// OS process id of the Chrome child this session owns.
    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }
    /// Connections the pinning proxy made, with the address it pinned for each.
    pub fn pinned_connections(&self) -> Vec<PinnedConnection> {
        self.proxy
            .as_ref()
            .map(|p| p.log.lock().unwrap().clone())
            .unwrap_or_default()
    }
    pub fn is_headless(&self) -> bool {
        self.headless
    }
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }
    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    /// Open a new page (tab), make it current, and navigate to `url`.
    pub async fn new_page(&self, url: &str) -> Result<Arc<BrowserPage>> {
        let inner = self
            .browser
            .lock()
            .await
            .new_page("about:blank")
            .await
            .map_err(cdp)?;
        let id = format!("p{}", self.page_seq.fetch_add(1, Ordering::Relaxed) + 1);
        let page = Arc::new(
            BrowserPage::attach(
                id.clone(),
                inner,
                self.upload_root.clone(),
                self.guard.clone(),
                {
                    #[cfg(windows)]
                    {
                        matches!(self.fixture_backend, Some(FixtureBackend::WinAppcontainer))
                    }
                    #[cfg(not(windows))]
                    {
                        false
                    }
                },
            )
            .await?,
        );
        self.pages.lock().unwrap().push(page.clone());
        *self.current.lock().unwrap() = Some(id);
        if url != "about:blank" {
            // The tab's own about:blank must have committed, or it discards this navigation.
            page.settle_initial_document().await;
            if let Err(error) = page.goto(url).await {
                // What Chrome itself said, when it said anything: a navigation that fails
                // inside the browser leaves no other trace on this side.
                return Err(match error {
                    crate::error::BrowserError::Cdp(text) => {
                        let stderr = self
                            .proc
                            .lock()
                            .unwrap()
                            .as_ref()
                            .map(|chrome| chrome.stderr_tail.text())
                            .unwrap_or_default();
                        crate::error::BrowserError::Cdp(format!("{text}; chrome stderr: {stderr}"))
                    }
                    other => other,
                });
            }
        }
        Ok(page)
    }

    pub fn pages(&self) -> Vec<Arc<BrowserPage>> {
        self.pages.lock().unwrap().clone()
    }

    pub fn page(&self, id: &str) -> Result<Arc<BrowserPage>> {
        self.pages
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id() == id)
            .cloned()
            .ok_or_else(|| BrowserError::UnknownPage(id.into()))
    }

    /// The page actions default to.
    pub fn current(&self) -> Result<Arc<BrowserPage>> {
        let id = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| BrowserError::UnknownPage("<none>".into()))?;
        self.page(&id)
    }

    /// Make `id` current and bring it to the front.
    pub async fn select_page(&self, id: &str) -> Result<Arc<BrowserPage>> {
        let p = self.page(id)?;
        p.bring_to_front().await?;
        *self.current.lock().unwrap() = Some(id.into());
        Ok(p)
    }

    pub async fn close_page(&self, id: &str) -> Result<()> {
        let p = self.page(id)?;
        self.pages.lock().unwrap().retain(|x| x.id() != id);
        {
            let mut cur = self.current.lock().unwrap();
            if cur.as_deref() == Some(id) {
                *cur = self
                    .pages
                    .lock()
                    .unwrap()
                    .last()
                    .map(|p| p.id().to_string());
            }
        }
        p.close_inner().await
    }

    /// Completed files currently in the download directory.
    pub fn downloads(&self) -> HashSet<PathBuf> {
        completed_files(&self.download_dir)
    }

    /// Wait for a download not in `before` to finish (no `.crdownload`, stable size).
    pub async fn wait_download(
        &self,
        before: &HashSet<PathBuf>,
        timeout: Duration,
    ) -> Result<PathBuf> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = completed_files(&self.download_dir);
            if let Some(p) = now.difference(before).next() {
                let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(100)).await;
                if std::fs::metadata(p).map(|m| m.len()).unwrap_or(1) == size {
                    return Ok(p.clone());
                }
            }
            if Instant::now() >= deadline {
                return Err(BrowserError::Timeout("download did not complete".into()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Set a W3C permission (`geolocation`, `notifications`, `clipboard-read`,
    /// `midi`, ...) to granted or denied for `origin`, or every origin when `None`.
    pub async fn set_permission(
        &self,
        origin: Option<&str>,
        name: &str,
        granted: bool,
    ) -> Result<()> {
        let mut params = SetPermissionParams::new(
            PermissionDescriptor::new(name),
            if granted {
                PermissionSetting::Granted
            } else {
                PermissionSetting::Denied
            },
        );
        params.origin = origin.map(str::to_string);
        self.browser
            .lock()
            .await
            .execute(params)
            .await
            .map_err(cdp)?;
        Ok(())
    }

    pub async fn reset_permissions(&self) -> Result<()> {
        self.browser
            .lock()
            .await
            .execute(ResetPermissionsParams::default())
            .await
            .map_err(cdp)?;
        Ok(())
    }

    /// Close Chrome gracefully: CDP `Browser.close` and up to 5 s for exit, then SIGTERM to
    /// the process group and up to 5 s more, then SIGKILL (job termination on Windows).
    /// Safe to call twice. A temporary profile is deleted once Chrome has exited.
    pub async fn shutdown(&self) {
        self.stop(false).await;
    }

    /// Stop the fixture browser process tree while retaining its profile until the fixture
    /// proxy and immutable source server have both been shut down.
    pub(crate) async fn stop_fixture_process(&self) -> std::result::Result<(), FixtureError> {
        self.stopping.store(true, Ordering::SeqCst);
        let mut status = self.fixture_stop_status.subscribe();
        let stop_result = loop {
            let lock = self.fixture_stop_lock.lock().await;
            let current = *status.borrow();
            match current {
                2 => break Ok(()),
                3 => {
                    let detail = self
                        .fixture_stop_failure
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                        .unwrap_or_else(|| "terminal stop worker gave no detail".into());
                    break Err(FixtureError::Cleanup(format!(
                        "browser termination previously failed without a stop receipt: {detail}"
                    )));
                }
                1 => {
                    drop(lock);
                    tokio::time::timeout(FIXTURE_STOP_RECEIPT_GRACE, status.changed())
                        .await
                        .map_err(|_| {
                            FixtureError::Cleanup(
                                "browser termination receipt did not arrive before deadline".into(),
                            )
                        })?
                        .map_err(|_| {
                            FixtureError::Cleanup(
                                "browser termination status channel closed".into(),
                            )
                        })?;
                }
                _ => {
                    self.fixture_stop_status.send_replace(1);
                    let process = self.proc.lock().unwrap().take();
                    let Some(mut process) = process else {
                        *self
                            .fixture_stop_failure
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = Some(
                            "browser process ownership is absent without a stop receipt".into(),
                        );
                        self.fixture_stop_status.send_replace(3);
                        break Err(FixtureError::Cleanup(
                            "browser process ownership is absent without a stop receipt".into(),
                        ));
                    };
                    process.begin_fixture_transport_shutdown();
                    // This transaction owns Chrome until it publishes a terminal
                    // receipt. The caller can time out or cancel during terminal
                    // fencing without abandoning status=1 or the process handle.
                    let process_slot = self.proc.clone();
                    let status_sender = self.fixture_stop_status.clone();
                    let failure_slot = self.fixture_stop_failure.clone();
                    tokio::spawn(async move {
                        let mut completion = FixtureStopCompletionGuard {
                            status: status_sender,
                            complete: false,
                        };
                        let terminated = tokio::task::spawn_blocking(move || {
                            let result = process.terminate_fixture_tree();
                            (process, result)
                        })
                        .await;
                        let (process, result) = match terminated {
                            Ok(result) => result,
                            Err(error) => {
                                *failure_slot
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
                                    format!("fixture browser termination worker failed: {error}"),
                                );
                                return;
                            }
                        };
                        if let Err(error) = result {
                            *failure_slot
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(format!(
                                "fixture browser process tree termination failed: {error}"
                            ));
                            *process_slot
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(process);
                            return;
                        }
                        #[cfg(windows)]
                        {
                            let mut process = process;
                            if let Err(error) = process
                                .finish_fixture_transport_shutdown(Instant::now() + GRACE)
                                .await
                            {
                                *failure_slot
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
                                    format!("fixture CDP bridge finalization failed: {error}"),
                                );
                                *process_slot
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner()) = Some(process);
                                return;
                            }
                        }
                        completion.stopped();
                    });
                    drop(lock);
                    tokio::time::timeout(FIXTURE_STOP_RECEIPT_GRACE, status.changed())
                        .await
                        .map_err(|_| {
                            FixtureError::Cleanup(
                                "browser termination receipt did not arrive before deadline".into(),
                            )
                        })?
                        .map_err(|_| {
                            FixtureError::Cleanup(
                                "browser termination status channel closed".into(),
                            )
                        })?;
                }
            }
        };
        stop_result?;
        for page in self.pages.lock().unwrap().drain(..) {
            page.abort_pumps();
        }
        if let Some(handler) = self.handler.lock().unwrap().take() {
            handler.abort();
        }
        if let Some(watcher) = self.watcher.lock().unwrap().take() {
            watcher.abort();
        }
        self.emit_stopped("fixture shutdown");
        Ok(())
    }

    pub(crate) fn process_alive(&self) -> bool {
        let Ok(mut process) = self.proc.lock() else {
            return false;
        };
        process
            .as_mut()
            .is_some_and(|process| process.has_exited().is_ok_and(|exited| !exited))
    }

    pub(crate) async fn stop_fixture_proxy(&self) -> std::result::Result<(), FixtureError> {
        if let Some(proxy) = &self.proxy {
            proxy
                .shutdown(GRACE)
                .await
                .map_err(|error| FixtureError::Cleanup(format!("fixture proxy: {error}")))?;
        }
        Ok(())
    }

    pub(crate) fn remove_fixture_profile(&self) -> std::result::Result<(), FixtureError> {
        let path = self.fixture_profile_path.as_ref().ok_or_else(|| {
            FixtureError::Cleanup("fixture browser did not own a disposable profile".into())
        })?;
        match self.profile_guard.lock().unwrap().take() {
            Some(ProfileGuard::Temp(dir)) => drop(dir),
            Some(_) => {
                return Err(FixtureError::Cleanup(
                    "fixture browser did not own a disposable profile".into(),
                ))
            }
            None => {}
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while path.exists()
            && std::fs::remove_dir_all(path).is_err()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        if path.exists() {
            Err(FixtureError::Cleanup(
                "disposable browser profile still exists after cleanup".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Cancel: kill the owned Chrome now and wait for it to exit. Helper
    /// processes exit with their parent; none are detached.
    pub async fn cancel(&self) {
        self.stop(true).await;
    }

    async fn stop(&self, hard: bool) {
        self.stop_inner(hard, true).await;
    }

    async fn stop_inner(&self, hard: bool, release_profile_dir: bool) {
        self.stopping.store(true, Ordering::SeqCst);
        if !hard {
            let graceful = async {
                let _ = self.browser.lock().await.close().await;
                let end = Instant::now() + GRACE;
                loop {
                    let done = match self.proc.lock().unwrap().as_mut() {
                        Some(p) => matches!(p.try_wait(), Ok(Some(_)) | Err(_)),
                        None => true,
                    };
                    if done || Instant::now() >= end {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(6), graceful).await;
        }
        // Whatever is left: SIGTERM the process group with a bounded wait, SIGKILL last
        // (cancel kills at once). Windows: the job object takes the whole tree.
        let taken = self.proc.lock().unwrap().take();
        if let Some(mut p) = taken {
            let grace = if hard { Duration::ZERO } else { GRACE };
            let _ = tokio::task::spawn_blocking(move || p.terminate_gracefully(grace)).await;
        }
        if release_profile_dir {
            release_profile(self.profile_guard.lock().unwrap().take());
        }
        for p in self.pages.lock().unwrap().drain(..) {
            p.abort_pumps();
        }
        if let Some(h) = self.handler.lock().unwrap().take() {
            h.abort();
        }
        if let Some(w) = self.watcher.lock().unwrap().take() {
            w.abort();
        }
        let reason = if hard { "cancelled" } else { "shutdown" };
        self.emit_stopped(reason);
    }

    fn emit_stopped(&self, reason: &str) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            let pid = self.pid;
            self.guard
                .emit_kind(LifecycleKind::Stop, reason, |e| e.pid = pid);
        }
    }
}

/// Emit at most one `Crash` per session, never during an intentional stop.
fn report_crash(
    guard: &Guard,
    stopping: &AtomicBool,
    crashed: &AtomicBool,
    status: Option<std::process::ExitStatus>,
    pid: Option<u32>,
) {
    if stopping.load(Ordering::SeqCst) || crashed.swap(true, Ordering::SeqCst) {
        return;
    }
    let reason = match status {
        Some(s) => format!("chrome exited unexpectedly ({s})"),
        None => "devtools connection lost".to_string(),
    };
    guard.emit_kind(LifecycleKind::Crash, reason, |e| e.pid = pid);
}

/// Exit status of the owned Chrome, if it has exited.
fn chrome_exit(proc: &StdMutex<Option<ChromeProc>>) -> Option<std::process::ExitStatus> {
    proc.lock()
        .ok()?
        .as_mut()
        .and_then(|p| p.try_wait().ok().flatten())
}

/// Fixture monitoring observes exit without reaping the process-group leader; cleanup must
/// retain that identity until `terminate_tree_bounded` has killed every descendant.
fn chrome_has_exited(proc: &StdMutex<Option<ChromeProc>>) -> bool {
    proc.lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|p| p.has_exited()))
        .and_then(std::result::Result::ok)
        .unwrap_or(true)
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        for p in self.pages.lock().unwrap().iter() {
            p.abort_pumps();
        }
        if let Some(h) = self.handler.lock().unwrap().take() {
            h.abort();
        }
        if let Some(w) = self.watcher.lock().unwrap().take() {
            w.abort();
        }
        if self.fixture_mode {
            if let Some(mut process) = self.proc.lock().unwrap().take() {
                let _ = process.terminate_fixture_tree();
            }
            if let Some(ProfileGuard::Temp(profile)) = self.profile_guard.lock().unwrap().take() {
                let _ = profile.keep();
            }
            return;
        }
        // An abandoned session (cancelled caller, panic, early return) still closes
        // gracefully: SIGTERM to Chrome's process group, a bounded wait, SIGKILL last.
        if let Some(mut p) = self.proc.lock().unwrap().take() {
            let _ = p.terminate_gracefully(GRACE);
        }
        // Chrome has exited: now the temporary profile can go.
        release_profile(self.profile_guard.lock().unwrap().take());
        self.emit_stopped("dropped");
    }
}

fn completed_files(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && !p
                            .extension()
                            .is_some_and(|e| e == "crdownload" || e == "tmp")
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_disable_the_component_updater() {
        // Chrome's component updater (clients2.google.com) runs even with background
        // networking off; a session must see only its pages' requests.
        assert!(BASE_CHROME_ARGS.contains(&"--disable-component-update"));
        assert!(BASE_CHROME_ARGS.contains(&"--disable-background-networking"));
    }

    #[test]
    fn profile_dir_accepts_spaces_and_preserves_caller_directory_on_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let name = format!(".ScrapeRight profile's ü & (data) {}", "x".repeat(65));
        assert!(validate_profile_name(&name).is_err());
        let path = temp.path().join(name);
        std::fs::create_dir(&path).unwrap();
        let sentinel = path.join("Preferences");
        std::fs::write(&sentinel, "caller data").unwrap();
        let opts = LaunchOptions::default().profile_dir(&path);
        assert_eq!(opts.profile, ProfileSpec::Directory(path.clone()));
        let (guard, resolved, downloads) = prepare_profile(&opts.profile).unwrap();
        assert!(matches!(&guard, ProfileGuard::Existing));
        assert_eq!(resolved, path);
        assert_eq!(downloads, path.join("rightkit-downloads"));
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        // shutdown/cancel/Drop all release this same guard through Option::take.
        let guard = StdMutex::new(Some(guard));
        drop(guard.lock().unwrap().take());
        drop(guard.lock().unwrap().take());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "caller data");
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    }

    #[test]
    fn profile_dir_rejects_relative_missing_and_file_paths_without_creation() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing profile");
        let file = temp.path().join("not a directory");
        std::fs::write(&file, "keep").unwrap();
        for (path, message) in [
            (PathBuf::from("relative profile"), "must be absolute"),
            (missing.clone(), "must exist"),
            (file.clone(), "not a directory"),
        ] {
            let opts = LaunchOptions::default().profile_dir(path);
            let err = match prepare_profile(&opts.profile) {
                Ok(_) => panic!("invalid profile was accepted"),
                Err(err) => err,
            };
            assert!(matches!(&err, BrowserError::Profile(_)));
            assert!(err.to_string().contains(message), "{err}");
        }
        assert!(!missing.exists());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
    }

    #[test]
    fn temporary_profile_still_uses_cleanup_guard() {
        let opts = LaunchOptions::default();
        assert_eq!(opts.profile, ProfileSpec::Temporary);
        let (guard, path, downloads) = prepare_profile(&opts.profile).unwrap();
        assert!(matches!(&guard, ProfileGuard::Temp(_)));
        assert_eq!(downloads, path.join("downloads"));
        assert!(path.is_dir());
        drop(guard);
        assert!(!path.exists());
    }

    // APFS (macOS) rejects non-UTF-8 file names with EILSEQ; Linux filesystems accept them.
    #[cfg(target_os = "linux")]
    #[test]
    fn profile_dir_accepts_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join(OsString::from_vec(b"profile \xff".to_vec()));
        std::fs::create_dir(&path).unwrap();
        let opts = LaunchOptions::default().profile_dir(&path);
        let (guard, resolved, _) = prepare_profile(&opts.profile).unwrap();
        assert_eq!(resolved, path);
        drop(guard);
        assert!(path.is_dir());
    }
}
