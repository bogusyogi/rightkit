//! The platform seam under [`super::PdfHost`].
//!
//! The host owns the policy (which window may reach which view, owner checks, the suspended-view
//! cache, counters, events). A backend owns the native objects and nothing else: it mints a
//! [`ViewId`] per view and does exactly what it is told with that id. It makes no authorization
//! decision and never sees a window it was not handed.

use super::{HostError, HostFrame, HostOutlineItem, HostViewMeta, NativeParent, UpdateRequest};

/// A native view the backend holds. Minted by the backend, meaningful only to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ViewId(pub(crate) u64);

/// What a view looks like, without the owner (the host adds it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewState {
    pub page: u32,
    pub num_pages: u32,
    pub scale: f64,
    pub fit: Option<String>,
    pub rotation: i32,
    pub panel: Option<String>,
    pub locked: bool,
    pub outline: Vec<HostOutlineItem>,
}

/// One open call. Only the PDFKit backend reads every field.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct BackendOpen<'a> {
    pub window: &'a str,
    pub owner: &'a str,
    pub path: &'a str,
    pub parent: &'a NativeParent,
    pub frame: HostFrame,
    pub reader_rgb: Option<[u8; 3]>,
    pub initial_view: Option<&'a HostViewMeta>,
    pub password: Option<&'a str>,
    /// A suspended view to reuse instead of loading the document again.
    pub reuse: Option<ViewId>,
}

/// A find query.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct FindQuery<'a> {
    pub text: &'a str,
    pub match_case: bool,
    pub whole_word: bool,
}

pub(crate) trait HostBackend: Send + Sync {
    /// False for the backend of a platform with no native view.
    fn supported(&self) -> bool;

    /// Load (or reuse) a document and attach its view next to `parent`.
    fn open(&self, request: &BackendOpen<'_>) -> Result<(ViewId, ViewState), HostError>;

    fn update(
        &self,
        view: ViewId,
        parent: &NativeParent,
        request: &UpdateRequest,
    ) -> Result<ViewState, HostError>;

    fn state(&self, view: ViewId) -> Result<ViewState, HostError>;

    /// Search, then make `pick(count)` the active match. Returns the match count and the active
    /// index (`None` for no match). With no match the highlight is cleared.
    fn find(
        &self,
        view: ViewId,
        query: &FindQuery<'_>,
        pick: &dyn Fn(usize) -> usize,
    ) -> Result<(usize, Option<usize>), HostError>;

    /// Clear the highlight and selection.
    fn clear_find(&self, view: ViewId) -> Result<(), HostError>;

    /// Run the print panel. Must not be called with any host lock held.
    fn print(&self, view: ViewId) -> Result<bool, HostError>;

    fn is_encrypted(&self, view: ViewId) -> bool;

    /// Hide a view for later reuse: remove its key monitor and side panel, give focus back to the
    /// webview when the view had it, clear the selection.
    fn park(&self, view: ViewId) -> Result<(), HostError>;

    /// Release a view, parked or active. Never fails; an unknown id is ignored.
    fn release(&self, view: ViewId, restore_focus: bool);
}

/// The backend of a platform with no native PDF view.
pub(crate) struct UnsupportedBackend;

impl HostBackend for UnsupportedBackend {
    fn supported(&self) -> bool {
        false
    }

    fn open(&self, _request: &BackendOpen<'_>) -> Result<(ViewId, ViewState), HostError> {
        Err(HostError::Unsupported)
    }

    fn update(
        &self,
        _view: ViewId,
        _parent: &NativeParent,
        _request: &UpdateRequest,
    ) -> Result<ViewState, HostError> {
        Err(HostError::Unsupported)
    }

    fn state(&self, _view: ViewId) -> Result<ViewState, HostError> {
        Err(HostError::Unsupported)
    }

    fn find(
        &self,
        _view: ViewId,
        _query: &FindQuery<'_>,
        _pick: &dyn Fn(usize) -> usize,
    ) -> Result<(usize, Option<usize>), HostError> {
        Err(HostError::Unsupported)
    }

    fn clear_find(&self, _view: ViewId) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn print(&self, _view: ViewId) -> Result<bool, HostError> {
        Err(HostError::Unsupported)
    }

    fn is_encrypted(&self, _view: ViewId) -> bool {
        false
    }

    fn park(&self, _view: ViewId) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn release(&self, _view: ViewId, _restore_focus: bool) {}
}
