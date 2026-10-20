//! Host lifecycle, isolation and refusal tests over a fake backend (no native code runs).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::backend::{BackendOpen, FindQuery, HostBackend, ViewId, ViewState};
use super::*;

// -- fixtures ---------------------------------------------------------------------------------

#[allow(dead_code)]
struct FakeView {
    owner: String,
    parked: bool,
    panel: Option<String>,
}

#[derive(Default)]
struct FakeState {
    next: u64,
    views: HashMap<u64, FakeView>,
    created: u64,
    reused: u64,
    released: Vec<(u64, bool)>,
    fail_open: bool,
    fail_park: bool,
    encrypted: bool,
    hits: usize,
    cleared: u32,
    printed: u32,
    updates: u32,
    owners_seen_by_open: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }

    fn live(&self) -> usize {
        self.with(|s| s.views.len())
    }

    fn created(&self) -> u64 {
        self.with(|s| s.created)
    }

    fn reused(&self) -> u64 {
        self.with(|s| s.reused)
    }
}

fn view_state(panel: Option<String>) -> ViewState {
    ViewState {
        page: 1,
        num_pages: 10,
        scale: 1.0,
        fit: None,
        rotation: 0,
        panel,
        locked: false,
        outline: Vec::new(),
    }
}

impl HostBackend for Fake {
    fn supported(&self) -> bool {
        true
    }

    fn open(&self, request: &BackendOpen<'_>) -> Result<(ViewId, ViewState), HostError> {
        self.with(|s| {
            if s.fail_open {
                return Err(HostError::Load("fake load failure".into()));
            }
            s.owners_seen_by_open.push(request.owner.to_owned());
            if let Some(ViewId(id)) = request.reuse {
                let view = s.views.get_mut(&id).filter(|v| v.parked).ok_or_else(|| {
                    HostError::Native("reuse of a view that is not parked".into())
                })?;
                view.parked = false;
                view.owner = request.owner.to_owned();
                s.reused += 1;
                return Ok((ViewId(id), view_state(None)));
            }
            s.next += 1;
            let id = s.next;
            s.created += 1;
            s.views.insert(
                id,
                FakeView {
                    owner: request.owner.to_owned(),
                    parked: false,
                    panel: None,
                },
            );
            Ok((ViewId(id), view_state(None)))
        })
    }

    fn update(
        &self,
        view: ViewId,
        _parent: &NativeParent,
        request: &UpdateRequest,
    ) -> Result<ViewState, HostError> {
        self.with(|s| {
            s.updates += 1;
            let v = s.views.get_mut(&view.0).ok_or(HostError::NotOpen)?;
            if let Some(panel) = &request.panel {
                v.panel = (!panel.is_empty() && panel != "none").then(|| panel.clone());
            }
            Ok(view_state(v.panel.clone()))
        })
    }

    fn state(&self, view: ViewId) -> Result<ViewState, HostError> {
        self.with(|s| {
            let v = s.views.get(&view.0).ok_or(HostError::NotOpen)?;
            Ok(view_state(v.panel.clone()))
        })
    }

    fn find(
        &self,
        view: ViewId,
        _query: &FindQuery<'_>,
        pick: &dyn Fn(usize) -> usize,
    ) -> Result<(usize, Option<usize>), HostError> {
        self.with(|s| {
            s.views.get(&view.0).ok_or(HostError::NotOpen)?;
            if s.hits == 0 {
                return Ok((0, None));
            }
            Ok((s.hits, Some(pick(s.hits))))
        })
    }

    fn clear_find(&self, view: ViewId) -> Result<(), HostError> {
        self.with(|s| {
            s.views.get(&view.0).ok_or(HostError::NotOpen)?;
            s.cleared += 1;
            Ok(())
        })
    }

    fn print(&self, view: ViewId) -> Result<bool, HostError> {
        self.with(|s| {
            s.views.get(&view.0).ok_or(HostError::NotOpen)?;
            s.printed += 1;
            Ok(true)
        })
    }

    fn is_encrypted(&self, _view: ViewId) -> bool {
        self.with(|s| s.encrypted)
    }

    fn park(&self, view: ViewId) -> Result<(), HostError> {
        self.with(|s| {
            if s.fail_park {
                return Err(HostError::Native("fake park failure".into()));
            }
            s.views.get_mut(&view.0).ok_or(HostError::NotOpen)?.parked = true;
            Ok(())
        })
    }

    fn release(&self, view: ViewId, restore_focus: bool) {
        self.with(|s| {
            if s.views.remove(&view.0).is_some() {
                s.released.push((view.0, restore_focus));
            }
        });
    }
}

/// A clock the test moves.
#[derive(Clone)]
struct TestClock(Arc<Mutex<Instant>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    fn reader(&self) -> Box<dyn Fn() -> Instant + Send + Sync> {
        let inner = Arc::clone(&self.0);
        Box::new(move || *inner.lock().unwrap())
    }
}

fn host_with(fake: &Fake, config: HostConfig) -> (PdfHost, TestClock) {
    let clock = TestClock::new();
    let host = PdfHost::with_backend_and_clock(Box::new(fake.clone()), config, clock.reader());
    (host, clock)
}

fn host(fake: &Fake) -> (PdfHost, TestClock) {
    host_with(fake, HostConfig::default())
}

static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempPdf {
    path: PathBuf,
}

impl TempPdf {
    fn new(contents: &[u8]) -> Self {
        let n = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rightkit-pdf-host-test-{}-{n}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        Self { path }
    }

    fn rewrite(&self, contents: &[u8]) {
        std::fs::write(&self.path, contents).unwrap();
    }

    fn path_string(&self) -> String {
        self.path.to_str().unwrap().to_owned()
    }
}

impl Drop for TempPdf {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn frame() -> HostFrame {
    HostFrame {
        x: 10.0,
        y: 20.0,
        width: 600.0,
        height: 400.0,
    }
}

fn request(owner: &str, file: &TempPdf) -> OpenRequest {
    OpenRequest {
        owner: owner.to_owned(),
        path: AuthorizedPdfPath::from_authorized(&file.path).unwrap(),
        frame: frame(),
        reader_rgb: None,
        initial_view: None,
        password: None,
    }
}

fn open(host: &PdfHost, window: &str, owner: &str, file: &TempPdf) -> Result<HostState, HostError> {
    host.open(window, &NativeParent::fake(), request(owner, file))
}

fn phases(events: &[HostResourceEvent]) -> Vec<(ResourcePhase, String)> {
    events.iter().map(|e| (e.phase, e.reason.clone())).collect()
}

// -- open and state ---------------------------------------------------------------------------

#[test]
fn open_reports_the_state_and_an_active_event() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let file = TempPdf::new(b"%PDF-1.7 one");
    let state = open(&host, "main", "tab-1", &file).unwrap();
    assert_eq!(state.owner, "tab-1");
    assert_eq!(state.num_pages, 10);
    assert!(!state.has_outline);
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 1,
            parked: 0,
            released: 0
        }
    );
    let events = host.drain_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, ResourcePhase::Active);
    assert_eq!(events[0].reason, "open");
    assert_eq!(events[0].window, "main");
    assert_eq!(events[0].path, file.path_string());
    assert!(events[0]
        .identity
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(events[0].active_count, 1);
    // The outbox is drained.
    assert!(host.drain_events().is_empty());
    assert_eq!(host.state("main", "tab-1").unwrap(), state);
}

#[test]
fn opening_again_replaces_the_window_view_and_leaks_nothing() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let b = TempPdf::new(b"%PDF b");
    open(&host, "main", "tab-1", &a).unwrap();
    open(&host, "main", "tab-2", &b).unwrap();
    assert_eq!(fake.live(), 1);
    assert_eq!(fake.created(), 2);
    assert_eq!(host.counts().released, 1);
    // The first owner no longer matches.
    assert_eq!(host.state("main", "tab-1"), Err(HostError::StaleOwner));
    assert!(host.state("main", "tab-2").is_ok());
    assert_eq!(
        phases(&host.drain_events()),
        vec![
            (ResourcePhase::Active, "open".into()),
            (ResourcePhase::Released, "open-replaced".into()),
            (ResourcePhase::Active, "open".into()),
        ]
    );
    // The replaced view gets its focus back (the page asked for the swap).
    assert_eq!(fake.with(|s| s.released.clone()), vec![(1, true)]);
}

#[test]
fn a_failed_open_keeps_the_previous_view() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let b = TempPdf::new(b"%PDF b");
    open(&host, "main", "tab-1", &a).unwrap();
    fake.with(|s| s.fail_open = true);
    let error = open(&host, "main", "tab-2", &b).unwrap_err();
    assert_eq!(error.code(), "load_failed");
    assert!(host.state("main", "tab-1").is_ok());
    assert_eq!(fake.live(), 1);
    assert_eq!(host.counts().released, 0);
}

#[test]
fn a_failed_reopen_releases_the_view_it_took_from_the_cache() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    assert_eq!(host.counts().parked, 1);
    fake.with(|s| s.fail_open = true);
    assert!(open(&host, "main", "tab-1", &a).is_err());
    assert_eq!(fake.live(), 0, "the reused view must not leak");
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 0,
            parked: 0,
            released: 1
        }
    );
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.phase == ResourcePhase::Released && e.reason == "open-failed"));
}

// -- owner isolation --------------------------------------------------------------------------

#[test]
fn a_window_cannot_reach_another_windows_document() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF secret of a");
    let b = TempPdf::new(b"%PDF b");
    open(&host, "win-a", "owner-1", &a).unwrap();
    open(&host, "win-b", "owner-2", &b).unwrap();
    let a_path = a.path_string();
    let request = FindRequest {
        query: Some("secret".into()),
        ..FindRequest::default()
    };

    // A window with no view of its own finds nothing, even with the right owner token.
    assert_eq!(host.state("win-c", "owner-1"), Err(HostError::NotOpen));
    assert_eq!(
        host.find("win-c", "owner-1", &request),
        Err(HostError::NotOpen)
    );
    assert_eq!(host.print("win-c", "owner-1"), Err(HostError::NotOpen));
    assert_eq!(
        host.update(
            "win-c",
            &NativeParent::fake(),
            "owner-1",
            &UpdateRequest::default()
        ),
        Err(HostError::NotOpen)
    );
    assert_eq!(
        host.suspend("win-c", "owner-1", &a_path),
        Err(HostError::NotOpen)
    );

    // A window with its own view cannot use another window's owner token.
    assert_eq!(host.state("win-b", "owner-1"), Err(HostError::StaleOwner));
    assert_eq!(
        host.find("win-b", "owner-1", &request),
        Err(HostError::StaleOwner)
    );
    assert_eq!(host.print("win-b", "owner-1"), Err(HostError::StaleOwner));

    // Nothing reached the native layer for any of those.
    assert_eq!(fake.with(|s| (s.printed, s.updates)), (0, 0));

    // Closing by another window's path closes nothing of either window.
    host.close("win-b", None, Some(&a_path), "attack").unwrap();
    host.close("win-c", None, Some(&a_path), "attack").unwrap();
    assert_eq!(host.state("win-a", "owner-1").unwrap().owner, "owner-1");
    assert_eq!(host.state("win-b", "owner-2").unwrap().owner, "owner-2");
    assert_eq!(fake.live(), 2);

    // A suspended view belongs to its window too: window b's open of the same path does not take it.
    host.suspend("win-a", "owner-1", &a_path).unwrap();
    open(&host, "win-b", "owner-2", &a).unwrap();
    assert_eq!(fake.reused(), 0);
    assert_eq!(host.counts().parked, 1);
    host.release_window("win-a");
    assert_eq!(host.counts().parked, 0);
}

#[test]
fn a_stale_owner_changes_nothing() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    let path = a.path_string();
    let stale = HostError::StaleOwner;
    assert_eq!(host.state("main", "tab-2"), Err(stale.clone()));
    assert_eq!(
        host.update(
            "main",
            &NativeParent::fake(),
            "tab-2",
            &UpdateRequest::default()
        ),
        Err(stale.clone())
    );
    assert_eq!(
        host.find("main", "tab-2", &FindRequest::default()),
        Err(stale.clone())
    );
    assert_eq!(host.print("main", "tab-2"), Err(stale.clone()));
    assert_eq!(host.suspend("main", "tab-2", &path), Err(stale.clone()));
    assert_eq!(
        host.suspend("main", "tab-1", "/another.pdf"),
        Err(stale.clone())
    );
    assert_eq!(host.close("main", Some("tab-2"), None, "x"), Err(stale));
    assert_eq!(fake.live(), 1);
    assert_eq!(host.counts().released, 0);
    assert!(host.state("main", "tab-1").is_ok());
}

// -- refusals ---------------------------------------------------------------------------------

#[test]
fn malformed_requests_are_refused_before_the_native_layer() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let parent = NativeParent::fake();

    let long_window = "w".repeat(MAX_WINDOW_LABEL_BYTES + 1);
    for window in ["", "bad\nlabel", long_window.as_str()] {
        assert_eq!(
            open(&host, window, "tab", &a).unwrap_err().code(),
            "invalid",
            "{window:?}"
        );
    }
    let long_owner = "o".repeat(MAX_OWNER_BYTES + 1);
    for owner in ["", "bad\towner", long_owner.as_str()] {
        assert_eq!(
            open(&host, "main", owner, &a).unwrap_err().code(),
            "invalid"
        );
    }
    for bad in [
        HostFrame {
            x: f64::NAN,
            ..frame()
        },
        HostFrame {
            width: -1.0,
            ..frame()
        },
        HostFrame {
            height: f64::INFINITY,
            ..frame()
        },
        HostFrame {
            x: MAX_FRAME_EXTENT * 2.0,
            ..frame()
        },
    ] {
        let mut req = request("tab", &a);
        req.frame = bad;
        assert_eq!(
            host.open("main", &parent, req).unwrap_err().code(),
            "invalid"
        );
    }
    let mut req = request("tab", &a);
    req.initial_view = Some(HostViewMeta {
        scale: Some(f64::NAN),
        ..HostViewMeta::default()
    });
    assert_eq!(
        host.open("main", &parent, req).unwrap_err().code(),
        "invalid"
    );
    let mut req = request("tab", &a);
    req.initial_view = Some(HostViewMeta {
        fit: Some("cover".into()),
        ..HostViewMeta::default()
    });
    assert_eq!(
        host.open("main", &parent, req).unwrap_err().code(),
        "invalid"
    );
    let mut req = request("tab", &a);
    req.password = Some(PdfPassword::new("p".repeat(MAX_PASSWORD_BYTES + 1)));
    assert_eq!(
        host.open("main", &parent, req).unwrap_err().code(),
        "invalid"
    );
    assert_eq!(fake.created(), 0);

    open(&host, "main", "tab", &a).unwrap();
    for update in [
        UpdateRequest {
            scale: Some(0.0),
            ..UpdateRequest::default()
        },
        UpdateRequest {
            scale: Some(f64::INFINITY),
            ..UpdateRequest::default()
        },
        UpdateRequest {
            scale: Some(1000.0),
            ..UpdateRequest::default()
        },
        UpdateRequest {
            fit: Some("cover".into()),
            ..UpdateRequest::default()
        },
        UpdateRequest {
            panel: Some("../../etc".into()),
            ..UpdateRequest::default()
        },
        UpdateRequest {
            frame: Some(HostFrame {
                width: f64::NAN,
                ..frame()
            }),
            ..UpdateRequest::default()
        },
    ] {
        assert_eq!(
            host.update("main", &parent, "tab", &update)
                .unwrap_err()
                .code(),
            "invalid",
            "{update:?}"
        );
    }
    assert_eq!(fake.with(|s| s.updates), 0);
    let long = FindRequest {
        query: Some("q".repeat(MAX_FIND_QUERY_BYTES + 1)),
        ..FindRequest::default()
    };
    assert_eq!(
        host.find("main", "tab", &long).unwrap_err().code(),
        "invalid"
    );
    assert_eq!(
        host.close("main", None, None, "x").unwrap_err().code(),
        "invalid"
    );
    assert_eq!(
        host.close("main", Some(""), Some(""), "x")
            .unwrap_err()
            .code(),
        "invalid"
    );
}

#[test]
fn good_updates_reach_the_view() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab", &a).unwrap();
    let parent = NativeParent::fake();
    let state = host
        .update(
            "main",
            &parent,
            "tab",
            &UpdateRequest {
                panel: Some("thumbs".into()),
                fit: Some("width".into()),
                scale: Some(1.5),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
    assert_eq!(state.panel.as_deref(), Some("thumbs"));
    assert_eq!(state.owner, "tab");
    assert_eq!(fake.with(|s| s.updates), 1);
}

#[test]
fn paths_must_be_absolute_utf8_and_files() {
    assert!(AuthorizedPdfPath::from_authorized("relative/a.pdf").is_err());
    assert!(AuthorizedPdfPath::from_authorized("").is_err());
    assert!(AuthorizedPdfPath::from_authorized("a\0.pdf").is_err());
    let fake = Fake::default();
    let (host, _) = host_with(
        &fake,
        HostConfig {
            max_file_bytes: 16,
            ..HostConfig::default()
        },
    );
    // Missing file.
    let missing = std::env::temp_dir().join("rightkit-pdf-host-test-missing-file.pdf");
    let req = OpenRequest {
        owner: "tab".into(),
        path: AuthorizedPdfPath::from_authorized(&missing).unwrap(),
        frame: frame(),
        reader_rgb: None,
        initial_view: None,
        password: None,
    };
    assert_eq!(
        host.open("main", &NativeParent::fake(), req)
            .unwrap_err()
            .code(),
        "file"
    );
    // A directory is not a file.
    let req = OpenRequest {
        owner: "tab".into(),
        path: AuthorizedPdfPath::from_authorized(std::env::temp_dir()).unwrap(),
        frame: frame(),
        reader_rgb: None,
        initial_view: None,
        password: None,
    };
    assert_eq!(
        host.open("main", &NativeParent::fake(), req)
            .unwrap_err()
            .code(),
        "file"
    );
    // Over the size limit.
    let big = TempPdf::new(&[b'x'; 64]);
    assert_eq!(open(&host, "main", "tab", &big).unwrap_err().code(), "file");
    assert_eq!(fake.created(), 0);
}

#[test]
fn an_unsupported_host_answers_a_typed_error_for_every_call() {
    let host = PdfHost::unsupported();
    assert!(!host.supported());
    let a = TempPdf::new(b"%PDF a");
    let parent = NativeParent::detached();
    assert!(parent.is_detached());
    let all = [
        host.open("main", &parent, request("tab", &a)).map(|_| ()),
        host.update("main", &parent, "tab", &UpdateRequest::default())
            .map(|_| ()),
        host.state("main", "tab").map(|_| ()),
        host.find("main", "tab", &FindRequest::default())
            .map(|_| ()),
        host.print("main", "tab").map(|_| ()),
        host.suspend("main", "tab", &a.path_string()),
        host.close("main", Some("tab"), None, "x"),
    ];
    for result in all {
        let error = result.unwrap_err();
        assert_eq!(error, HostError::Unsupported);
        assert_eq!(error.code(), "unsupported");
    }
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 0,
            parked: 0,
            released: 0
        }
    );
    // Teardown on an empty host is harmless.
    host.release_window("main");
    host.dispose_all();
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_platform_host_is_the_unsupported_host() {
    let host = PdfHost::new(HostConfig::default(), HostHooks::default()).unwrap();
    assert!(!host.supported());
    let a = TempPdf::new(b"%PDF a");
    assert_eq!(
        host.open("main", &NativeParent::detached(), request("tab", &a))
            .unwrap_err(),
        HostError::Unsupported
    );
}

#[test]
fn a_config_out_of_bounds_does_not_build() {
    let bad = [
        HostConfig {
            parked_limit: 65,
            ..HostConfig::default()
        },
        HostConfig {
            parked_max_age: Duration::from_secs(601),
            ..HostConfig::default()
        },
        HostConfig {
            max_file_bytes: 0,
            ..HostConfig::default()
        },
        HostConfig {
            thumbnail_rail: f64::NAN,
            ..HostConfig::default()
        },
        HostConfig {
            thumbnail_rail: -1.0,
            ..HostConfig::default()
        },
    ];
    for config in bad {
        assert!(config.validate().is_err(), "{config:?}");
        assert!(PdfHost::new(config, HostHooks::default()).is_err());
    }
    assert!(HostConfig::default().validate().is_ok());
}

// -- suspend, reuse, cache bounds -------------------------------------------------------------

#[test]
fn a_suspended_view_is_reused_while_the_file_is_unchanged() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    let first_generation = host.drain_events()[0].generation;
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 0,
            parked: 1,
            released: 0
        }
    );
    assert_eq!(fake.live(), 1);
    // Parked views answer nothing.
    assert_eq!(host.state("main", "tab-1"), Err(HostError::NotOpen));
    // Reopening with a new owner reuses the view and keeps its generation.
    let state = open(&host, "main", "tab-9", &a).unwrap();
    assert_eq!(state.owner, "tab-9");
    assert_eq!(fake.created(), 1);
    assert_eq!(fake.reused(), 1);
    assert_eq!(fake.live(), 1);
    let events = host.drain_events();
    assert_eq!(
        phases(&events),
        vec![
            (ResourcePhase::Parked, "suspend".into()),
            (ResourcePhase::Active, "resume".into()),
        ]
    );
    assert_eq!(events[1].generation, first_generation);
}

#[test]
fn a_changed_file_is_not_reused() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    a.rewrite(b"%PDF a, edited elsewhere");
    open(&host, "main", "tab-1", &a).unwrap();
    assert_eq!(fake.reused(), 0);
    assert_eq!(fake.created(), 2);
    assert_eq!(fake.live(), 1, "the stale cached view must be released");
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.phase == ResourcePhase::Released && e.reason == "identity-mismatch"));
}

#[test]
fn encrypted_views_are_released_not_parked() {
    let fake = Fake::default();
    fake.with(|s| s.encrypted = true);
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 0,
            parked: 0,
            released: 1
        }
    );
    assert_eq!(fake.live(), 0);
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.reason == "encrypted" && e.phase == ResourcePhase::Released));
}

#[test]
fn a_view_that_cannot_be_parked_is_released() {
    let fake = Fake::default();
    fake.with(|s| s.fail_park = true);
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    assert_eq!(fake.live(), 0);
    assert_eq!(host.counts().parked, 0);
}

#[test]
fn the_cache_is_bounded_in_count_and_age() {
    let fake = Fake::default();
    let (host, clock) = host_with(
        &fake,
        HostConfig {
            parked_limit: 2,
            parked_max_age: Duration::from_secs(30),
            ..HostConfig::default()
        },
    );
    let files: Vec<TempPdf> = (0..3)
        .map(|i| TempPdf::new(format!("%PDF {i}").as_bytes()))
        .collect();
    for (i, file) in files.iter().enumerate() {
        let window = format!("w{i}");
        open(&host, &window, "tab", file).unwrap();
        host.suspend(&window, "tab", &file.path_string()).unwrap();
        clock.advance(Duration::from_secs(1));
    }
    // The oldest was evicted when the third arrived.
    assert_eq!(host.counts().parked, 2);
    assert_eq!(fake.live(), 2);
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.window == "w0" && e.reason == "cache-evicted"));
    let wait = host.next_sweep_in().unwrap();
    assert!(
        wait <= Duration::from_secs(30) && wait >= Duration::from_secs(25),
        "{wait:?}"
    );
    assert_eq!(host.sweep(), 0);

    // Age: past the limit everything parked goes, with no timer involved.
    clock.advance(Duration::from_secs(31));
    assert_eq!(host.sweep(), 2);
    assert_eq!(fake.live(), 0);
    assert_eq!(host.counts().parked, 0);
    assert_eq!(host.next_sweep_in(), None);
}

#[test]
fn an_expired_view_is_not_reused() {
    let fake = Fake::default();
    let (host, clock) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab", &a).unwrap();
    host.suspend("main", "tab", &a.path_string()).unwrap();
    clock.advance(Duration::from_secs(31));
    open(&host, "main", "tab", &a).unwrap();
    assert_eq!(fake.reused(), 0);
    assert_eq!(fake.live(), 1);
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.reason == "age-expired" && e.phase == ResourcePhase::Released));
}

#[test]
fn different_paths_of_one_window_are_cached_separately() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let b = TempPdf::new(b"%PDF b");
    // One window parks two different documents in turn; each stays cached under its own path.
    open(&host, "main", "t", &a).unwrap();
    host.suspend("main", "t", &a.path_string()).unwrap();
    fake.with(|s| s.fail_open = false);
    open(&host, "main", "t", &b).unwrap();
    host.suspend("main", "t", &b.path_string()).unwrap();
    assert_eq!(host.counts().parked, 2);
    assert_eq!(fake.live(), 2);
}

// -- close and teardown -----------------------------------------------------------------------

#[test]
fn close_by_owner_by_path_and_the_reason() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let b = TempPdf::new(b"%PDF b");
    open(&host, "main", "tab-1", &a).unwrap();
    host.drain_events();
    host.close("main", Some("tab-1"), None, "user-closed")
        .unwrap();
    assert_eq!(fake.live(), 0);
    let events = host.drain_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason, "user-closed");
    // A second close finds nothing and is not an error (the page may race a teardown).
    host.close("main", Some("tab-1"), None, "user-closed")
        .unwrap();

    // By path, with no owner: closes the active view and any parked view of that path.
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    open(&host, "main", "tab-2", &b).unwrap();
    host.close("main", None, Some(&a.path_string()), "tab-removed")
        .unwrap();
    assert_eq!(host.counts().parked, 0, "the cached view of that path went");
    assert!(
        host.state("main", "tab-2").is_ok(),
        "the active view of another path stayed"
    );

    // An owner mismatch is refused unless a cached view was released.
    assert_eq!(
        host.close("main", Some("tab-1"), None, "x"),
        Err(HostError::StaleOwner)
    );
    // A hostile reason is replaced, never echoed.
    host.close("main", Some("tab-2"), None, "bad\nreason")
        .unwrap();
    assert!(host
        .drain_events()
        .iter()
        .any(|e| e.reason == "terminal-close"));
}

#[test]
fn closing_a_parked_owner_in_the_window_releases_it() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let b = TempPdf::new(b"%PDF b");
    open(&host, "main", "tab-1", &a).unwrap();
    host.suspend("main", "tab-1", &a.path_string()).unwrap();
    open(&host, "main", "tab-2", &b).unwrap();
    // tab-1 is parked; closing it by owner releases the parked view and does not touch tab-2.
    host.close("main", Some("tab-1"), None, "tab-removed")
        .unwrap();
    assert_eq!(host.counts().parked, 0);
    assert!(host.state("main", "tab-2").is_ok());
}

#[test]
fn window_teardown_leaks_nothing() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let files: Vec<TempPdf> = (0..4)
        .map(|i| TempPdf::new(format!("%PDF t{i}").as_bytes()))
        .collect();
    // w0: active; w1: parked; w2: active + parked; w3: active.
    open(&host, "w0", "t", &files[0]).unwrap();
    open(&host, "w1", "t", &files[1]).unwrap();
    host.suspend("w1", "t", &files[1].path_string()).unwrap();
    open(&host, "w2", "t", &files[2]).unwrap();
    host.suspend("w2", "t", &files[2].path_string()).unwrap();
    open(&host, "w2", "t", &files[3]).unwrap();
    open(&host, "w3", "t", &files[0]).unwrap();
    assert_eq!(fake.live(), 5);

    host.release_window("w1");
    assert_eq!(fake.live(), 4);
    // The reaper releases every window the platform no longer lists.
    host.reap(&["w0", "w2"]);
    assert_eq!(fake.live(), 3, "w3's active view went; w0 and w2 stayed");
    assert!(host.state("w3", "t").is_err());
    assert!(host.state("w0", "t").is_ok());

    host.dispose_all();
    assert_eq!(fake.live(), 0);
    assert_eq!(
        host.counts(),
        HostCounts {
            active: 0,
            parked: 0,
            released: fake.created()
        }
    );
    // Open/close pairs balance: every native view created was released exactly once.
    assert_eq!(fake.with(|s| s.released.len() as u64), fake.created());
}

#[test]
fn repeated_open_close_cycles_do_not_grow() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    for i in 0..50 {
        let owner = format!("tab-{i}");
        open(&host, "main", &owner, &a).unwrap();
        host.close("main", Some(&owner), None, "cycle").unwrap();
    }
    assert_eq!(fake.live(), 0);
    assert_eq!(host.counts().released, 50);
    // Two events per cycle were recorded; the outbox never exceeds its cap.
    assert!(host.drain_events().len() <= EVENT_OUTBOX_CAP);
}

#[test]
fn the_event_outbox_is_bounded() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    for i in 0..(EVENT_OUTBOX_CAP + 50) {
        let owner = format!("tab-{i}");
        open(&host, "main", &owner, &a).unwrap();
    }
    let events = host.drain_events();
    assert_eq!(events.len(), EVENT_OUTBOX_CAP);
    // The oldest were dropped: the last event is the newest open.
    assert_eq!(
        events.last().unwrap().owner,
        format!("tab-{}", EVENT_OUTBOX_CAP + 49)
    );
}

// -- find and print ---------------------------------------------------------------------------

#[test]
fn find_steps_through_matches_and_clears() {
    let fake = Fake::default();
    fake.with(|s| s.hits = 3);
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab", &a).unwrap();
    let start = FindRequest {
        query: Some("word".into()),
        ..FindRequest::default()
    };
    let step = |direction| FindRequest {
        direction: Some(direction),
        ..FindRequest::default()
    };
    let r = host.find("main", "tab", &start).unwrap();
    assert_eq!((r.count, r.active_index), (3, 0));
    assert_eq!(host.find("main", "tab", &step(1)).unwrap().active_index, 1);
    assert_eq!(host.find("main", "tab", &step(1)).unwrap().active_index, 2);
    assert_eq!(
        host.find("main", "tab", &step(1)).unwrap().active_index,
        0,
        "wraps forward"
    );
    assert_eq!(
        host.find("main", "tab", &step(-1)).unwrap().active_index,
        2,
        "wraps back"
    );
    // No direction repeats the active match.
    assert_eq!(
        host.find("main", "tab", &FindRequest::default())
            .unwrap()
            .active_index,
        2
    );

    let cleared = host
        .find(
            "main",
            "tab",
            &FindRequest {
                clear: Some(true),
                ..FindRequest::default()
            },
        )
        .unwrap();
    assert_eq!((cleared.count, cleared.active_index), (0, -1));
    assert_eq!(fake.with(|s| s.cleared), 1);
    // The query was forgotten.
    assert_eq!(
        host.find("main", "tab", &step(1)).unwrap_err().code(),
        "invalid"
    );
    // An empty query clears too.
    host.find("main", "tab", &start).unwrap();
    let empty = FindRequest {
        query: Some(String::new()),
        ..FindRequest::default()
    };
    assert_eq!(host.find("main", "tab", &empty).unwrap().count, 0);
    assert_eq!(fake.with(|s| s.cleared), 2);
}

#[test]
fn find_with_no_match_reports_none() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab", &a).unwrap();
    let r = host
        .find(
            "main",
            "tab",
            &FindRequest {
                query: Some("zzz".into()),
                ..FindRequest::default()
            },
        )
        .unwrap();
    assert_eq!((r.count, r.active_index), (0, -1));
}

#[test]
fn print_reaches_the_view_of_the_owner_only() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    open(&host, "main", "tab", &a).unwrap();
    assert_eq!(host.print("main", "tab"), Ok(true));
    assert_eq!(host.print("main", "other"), Err(HostError::StaleOwner));
    assert_eq!(fake.with(|s| s.printed), 1);
}

#[test]
fn pick_active_matches_the_donor_rules() {
    // No base: first, or last when going backwards; no step.
    assert_eq!(pick_active(5, None, None), 0);
    assert_eq!(pick_active(5, None, Some(1)), 0);
    assert_eq!(pick_active(5, None, Some(-1)), 4);
    // A base steps with wrap-around.
    assert_eq!(pick_active(5, Some(1), Some(1)), 2);
    assert_eq!(pick_active(5, Some(4), Some(1)), 0);
    assert_eq!(pick_active(5, Some(0), Some(-1)), 4);
    assert_eq!(pick_active(5, Some(3), Some(-1)), 2);
    // A base with no (or zero) direction holds, clamped.
    assert_eq!(pick_active(5, Some(3), None), 3);
    assert_eq!(pick_active(5, Some(3), Some(0)), 3);
    assert_eq!(pick_active(5, Some(99), None), 4);
    // The clamp comes before the step.
    assert_eq!(pick_active(5, Some(99), Some(1)), 0);
    assert_eq!(pick_active(1, Some(0), Some(1)), 0);
    assert_eq!(pick_active(0, Some(3), Some(1)), 0);
}

#[test]
fn rotation_and_page_helpers() {
    for (input, expected) in [
        (0, 0),
        (44, 0),
        (45, 90),
        (90, 90),
        (134, 90),
        (135, 180),
        (180, 180),
        (224, 180),
        (225, 270),
        (314, 270),
        (315, 0),
        (360, 0),
        (-90, 270),
        (-45, 0),
        (-46, 270),
        (i32::MIN, normalized_rotation(i32::MIN)),
    ] {
        assert_eq!(normalized_rotation(input), expected, "{input}");
    }
    assert_eq!(clamp_page(0, 10), 1);
    assert_eq!(clamp_page(7, 10), 7);
    assert_eq!(clamp_page(99, 10), 10);
    assert_eq!(clamp_page(5, 0), 1);
}

// -- wire shapes and secrets ------------------------------------------------------------------

#[test]
fn wire_shapes_are_camel_case_and_events_omit_the_window() {
    let fake = Fake::default();
    let (host, _) = host(&fake);
    let a = TempPdf::new(b"%PDF a");
    let state = open(&host, "main", "tab", &a).unwrap();
    let json = serde_json::to_value(&state).unwrap();
    for key in [
        "owner",
        "page",
        "numPages",
        "scale",
        "rotation",
        "locked",
        "hasOutline",
        "outline",
    ] {
        assert!(json.get(key).is_some(), "{key}");
    }
    let event = serde_json::to_value(&host.drain_events()[0]).unwrap();
    assert!(event.get("window").is_none());
    assert_eq!(event["phase"], "active");
    assert_eq!(event["activeCount"], 1);
    assert!(event.get("releasedCount").is_some());

    let update: UpdateRequest =
        serde_json::from_str(r#"{"readerRgb":[1,2,3],"page":4,"visible":false}"#).unwrap();
    assert_eq!(update.reader_rgb, Some([1, 2, 3]));
    assert_eq!(update.page, Some(4));
    let find: FindRequest =
        serde_json::from_str(r#"{"query":"x","matchCase":true,"wholeWord":false,"index":2}"#)
            .unwrap();
    assert_eq!((find.match_case, find.index), (Some(true), Some(2)));
}

#[test]
fn the_password_is_never_printed() {
    let password = PdfPassword::new("hunter2");
    assert!(!format!("{password:?}").contains("hunter2"));
    let a = TempPdf::new(b"%PDF a");
    let mut req = request("tab", &a);
    req.password = Some(PdfPassword::new("hunter2"));
    assert!(!format!("{req:?}").contains("hunter2"));
}

#[test]
fn the_default_hooks_refuse_links_and_claim_no_key() {
    let hooks = HostHooks::default();
    assert!(!hooks.open_link("https://example.com/"));
    let key = HostKey {
        owner: "tab".into(),
        key: "f".into(),
        code: "KeyF".into(),
        meta_key: true,
        ctrl_key: false,
        alt_key: false,
        shift_key: false,
    };
    assert!(!hooks.claim_key(&key));
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&opened);
    let hooks = HostHooks::default().with_open_link(move |url| {
        sink.lock().unwrap().push(url.to_owned());
        true
    });
    assert!(hooks.open_link("https://example.com/"));
    assert_eq!(opened.lock().unwrap().len(), 1);
}
