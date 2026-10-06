use crate::config::{find_chrome, validate_profile_name, LaunchOptions, ProfileSpec};
use crate::error::{cdp, BrowserError, Result};
use crate::page::BrowserPage;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::browser::{
    PermissionDescriptor, PermissionSetting, ResetPermissionsParams, SetDownloadBehaviorBehavior,
    SetDownloadBehaviorParams, SetPermissionParams,
};
use futures::StreamExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

enum ProfileGuard {
    Temp(#[allow(dead_code)] tempfile::TempDir),
    Named { _lock: std::fs::File },
}

/// One Chrome process with any number of pages.
pub struct BrowserSession {
    id: String,
    browser: Mutex<Browser>,
    pages: StdMutex<Vec<Arc<BrowserPage>>>,
    current: StdMutex<Option<String>>,
    page_seq: AtomicU64,
    handler: StdMutex<Option<JoinHandle<()>>>,
    _profile: ProfileGuard,
    profile_dir: PathBuf,
    download_dir: PathBuf,
    upload_root: Option<PathBuf>,
    headless: bool,
}

impl BrowserSession {
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        let exe = opts.chrome_path.clone().or_else(find_chrome).ok_or(BrowserError::ChromeNotFound)?;
        let (guard, profile_dir, default_dl) = match &opts.profile {
            ProfileSpec::Temporary => {
                let t = tempfile::tempdir()?;
                let p = t.path().to_path_buf();
                let dl = p.join("downloads");
                (ProfileGuard::Temp(t), p, dl)
            }
            ProfileSpec::Named { root, name } => {
                validate_profile_name(name)?;
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
                (ProfileGuard::Named { _lock: lock }, dir, dl)
            }
        };
        let download_dir = opts.download_dir.clone().unwrap_or(default_dl);
        std::fs::create_dir_all(&download_dir)?;

        let mut b = BrowserConfig::builder()
            .chrome_executable(exe)
            .user_data_dir(&profile_dir)
            .window_size(opts.viewport.0, opts.viewport.1)
            .launch_timeout(opts.launch_timeout)
            ;
        b = if opts.headless { b.new_headless_mode() } else { b.with_head() };
        if opts.mute_audio {
            b = b.arg("mute-audio");
        }
        for a in &opts.extra_args {
            b = b.arg(a.trim_start_matches("--").to_string());
        }
        let config = b.build().map_err(BrowserError::Launch)?;
        let (browser, mut handler) =
            Browser::launch(config).await.map_err(|e| BrowserError::Launch(e.to_string()))?;
        let pump = tokio::spawn(async move {
            while let Some(ev) = handler.next().await {
                if ev.is_err() {
                    break;
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

        Ok(Self {
            id: format!("bs-{}", SESSION_SEQ.fetch_add(1, Ordering::Relaxed)),
            browser: Mutex::new(browser),
            pages: StdMutex::new(Vec::new()),
            current: StdMutex::new(None),
            page_seq: AtomicU64::new(0),
            handler: StdMutex::new(Some(pump)),
            _profile: guard,
            profile_dir,
            download_dir,
            upload_root: opts.upload_root,
            headless: opts.headless,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
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
        let inner = self.browser.lock().await.new_page("about:blank").await.map_err(cdp)?;
        let id = format!("p{}", self.page_seq.fetch_add(1, Ordering::Relaxed) + 1);
        let page = Arc::new(BrowserPage::attach(id.clone(), inner, self.upload_root.clone()).await?);
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
        let id = self.current.lock().unwrap().clone().ok_or_else(|| BrowserError::UnknownPage("<none>".into()))?;
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
                *cur = self.pages.lock().unwrap().last().map(|p| p.id().to_string());
            }
        }
        p.close_inner().await
    }

    /// Completed files currently in the download directory.
    pub fn downloads(&self) -> HashSet<PathBuf> {
        completed_files(&self.download_dir)
    }

    /// Wait for a download not in `before` to finish (no `.crdownload`, stable size).
    pub async fn wait_download(&self, before: &HashSet<PathBuf>, timeout: Duration) -> Result<PathBuf> {
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
    pub async fn set_permission(&self, origin: Option<&str>, name: &str, granted: bool) -> Result<()> {
        let mut params = SetPermissionParams::new(
            PermissionDescriptor::new(name),
            if granted { PermissionSetting::Granted } else { PermissionSetting::Denied },
        );
        params.origin = origin.map(str::to_string);
        self.browser.lock().await.execute(params).await.map_err(cdp)?;
        Ok(())
    }

    pub async fn reset_permissions(&self) -> Result<()> {
        self.browser.lock().await.execute(ResetPermissionsParams::default()).await.map_err(cdp)?;
        Ok(())
    }

    /// Close Chrome. Safe to call twice. Temporary profiles are deleted on drop.
    pub async fn shutdown(&self) {
        let mut b = self.browser.lock().await;
        let _ = b.close().await;
        let _ = b.wait().await;
        for p in self.pages.lock().unwrap().drain(..) {
            p.abort_pumps();
        }
        if let Some(h) = self.handler.lock().unwrap().take() {
            h.abort();
        }
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        for p in self.pages.lock().unwrap().iter() {
            p.abort_pumps();
        }
        if let Some(h) = self.handler.lock().unwrap().take() {
            h.abort();
        }
    }
}

fn completed_files(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && !p.extension().is_some_and(|e| e == "crdownload" || e == "tmp")
                })
                .collect()
        })
        .unwrap_or_default()
}
