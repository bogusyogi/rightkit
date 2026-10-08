use crate::config::{
    reject_real_browser_profile, resolve_executable, validate_extra_args, validate_profile_dir,
    validate_profile_name, LaunchOptions, ProfileSpec,
};
use crate::error::{cdp, BrowserError, Result};
use crate::page::BrowserPage;
use crate::policy::{Guard, LifecycleKind};
use crate::proc::ChromeProc;
use crate::proxy::{PinProxy, PinnedConnection};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    PermissionDescriptor, PermissionSetting, ResetPermissionsParams, SetDownloadBehaviorBehavior,
    SetDownloadBehaviorParams, SetPermissionParams,
};
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

enum ProfileGuard {
    Temp(#[allow(dead_code)] tempfile::TempDir),
    Named {
        _lock: std::fs::File,
    },
    /// Caller-owned directory: dropping this guard has no filesystem effects.
    Existing,
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

fn prepare_profile(profile: &ProfileSpec) -> Result<(ProfileGuard, PathBuf, PathBuf)> {
    match profile {
        ProfileSpec::Temporary => {
            let t = tempfile::tempdir()?;
            let p = t.path().to_path_buf();
            let dl = p.join("downloads");
            Ok((ProfileGuard::Temp(t), p, dl))
        }
        ProfileSpec::Named { root, name } => {
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
                    return Err(BrowserError::ProfileInUse(name.clone()))
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            let dl = dir.join("rightkit-downloads");
            Ok((ProfileGuard::Named { _lock: lock }, dir, dl))
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
    download_dir: PathBuf,
    upload_root: Option<PathBuf>,
    headless: bool,
    guard: Arc<Guard>,
    pid: Option<u32>,
    proc: Arc<StdMutex<Option<ChromeProc>>>,
    proxy: Option<PinProxy>,
    /// Set before an intentional stop so the CDP pump does not report a crash.
    stopping: Arc<AtomicBool>,
    stopped: AtomicBool,
}

impl BrowserSession {
    pub async fn launch(
        #[cfg_attr(not(windows), allow(unused_mut))] mut opts: LaunchOptions,
    ) -> Result<Self> {
        validate_extra_args(&opts.extra_args)?;
        let exe = resolve_executable(&opts)?;
        let (profile_guard, profile_dir, default_dl) = prepare_profile(&opts.profile)?;
        let download_dir = opts.download_dir.clone().unwrap_or(default_dl);
        std::fs::create_dir_all(&download_dir)?;

        let id = format!("bs-{}", SESSION_SEQ.fetch_add(1, Ordering::Relaxed));
        let guard = Arc::new(Guard {
            session_id: id.clone(),
            admission: opts.admission.clone(),
            network: opts.network.clone(),
            events: opts.on_event.clone(),
        });
        let proxy = if opts.network.unrestricted {
            None
        } else {
            Some(PinProxy::start(guard.clone()).await?)
        };

        let mut args: Vec<OsString> = [
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
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
        ]
        .iter()
        .map(|s| OsString::from(*s))
        .collect();
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
        #[cfg(windows)]
        let job = opts.windows_job.take();
        #[cfg(not(windows))]
        let job = None;
        let proc = tokio::task::spawn_blocking(move || {
            crate::proc::spawn_chrome(&exe, &args, launch_timeout, job)
        })
        .await
        .map_err(|e| BrowserError::Launch(e.to_string()))??;
        let pid = Some(proc.child.id());
        let (browser, mut handler) = Browser::connect(proc.ws_url.clone())
            .await
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
        let proc = Arc::new(StdMutex::new(Some(proc)));
        let stopping = Arc::new(AtomicBool::new(false));
        let crashed = Arc::new(AtomicBool::new(false));
        let (pump_proc, pump_stopping, pump_guard, pump_crashed) = (
            proc.clone(),
            stopping.clone(),
            guard.clone(),
            crashed.clone(),
        );
        let pump = tokio::spawn(async move {
            // Transient handler errors are tolerated while Chrome is alive; the
            // stream ending (or Chrome exiting) ends the session.
            let mut errors = 0u32;
            while let Some(ev) = handler.next().await {
                if ev.is_err() {
                    errors += 1;
                    if errors > 50 || chrome_exit(&pump_proc).is_some() {
                        break;
                    }
                } else {
                    errors = 0;
                }
            }
            // Give the OS a moment to reap so the reason carries the exit status.
            let mut status = None;
            for _ in 0..25 {
                status = chrome_exit(&pump_proc);
                if status.is_some() || pump_stopping.load(Ordering::SeqCst) {
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
        let watcher = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if w_stopping.load(Ordering::SeqCst) || w_crashed.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(status) = chrome_exit(&w_proc) {
                    report_crash(&w_guard, &w_stopping, &w_crashed, Some(status), pid);
                    return;
                }
            }
        });
        browser
            .execute(
                SetDownloadBehaviorParams::builder()
                    .behavior(SetDownloadBehaviorBehavior::Allow)
                    .download_path(download_dir.to_string_lossy().to_string())
                    .build()
                    .map_err(BrowserError::Launch)?,
            )
            .await
            .map_err(cdp)?;

        let mode = if opts.headless { "headless" } else { "headed" };
        guard.emit_kind(LifecycleKind::Start, format!("launched ({mode})"), |e| {
            e.pid = pid
        });
        Ok(Self {
            id,
            browser: Mutex::new(browser),
            pages: StdMutex::new(Vec::new()),
            current: StdMutex::new(None),
            page_seq: AtomicU64::new(0),
            handler: StdMutex::new(Some(pump)),
            watcher: StdMutex::new(Some(watcher)),
            profile_guard: StdMutex::new(Some(profile_guard)),
            profile_dir,
            download_dir,
            upload_root: opts.upload_root,
            headless: opts.headless,
            guard,
            pid,
            proc,
            proxy,
            stopping,
            stopped: AtomicBool::new(false),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
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
            )
            .await?,
        );
        self.pages.lock().unwrap().push(page.clone());
        *self.current.lock().unwrap() = Some(id);
        if url != "about:blank" {
            page.goto(url).await?;
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

    /// Cancel: kill the owned Chrome now and wait for it to exit. Helper
    /// processes exit with their parent; none are detached.
    pub async fn cancel(&self) {
        self.stop(true).await;
    }

    async fn stop(&self, hard: bool) {
        self.stopping.store(true, Ordering::SeqCst);
        if !hard {
            let graceful = async {
                let _ = self.browser.lock().await.close().await;
                let end = Instant::now() + GRACE;
                loop {
                    let done = match self.proc.lock().unwrap().as_mut() {
                        Some(p) => matches!(p.child.try_wait(), Ok(Some(_)) | Err(_)),
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
        release_profile(self.profile_guard.lock().unwrap().take());
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
        .and_then(|p| p.child.try_wait().ok().flatten())
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
        // An abandoned session (cancelled caller, panic, early return) still closes
        // gracefully: SIGTERM to Chrome's process group, a bounded wait, SIGKILL last.
        if let Some(mut p) = self.proc.lock().unwrap().take() {
            p.terminate_gracefully(GRACE);
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
