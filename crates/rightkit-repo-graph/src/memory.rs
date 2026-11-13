//! Resident memory discipline for whole-graph work (#635).
//!
//! A large repository's graph is millions of heap objects. Two things kept the
//! resident engine at several GB: concurrent whole-graph loads each held a full
//! copy at once, and the allocator never handed a finished load or a failed
//! build's freed pages back to the OS, so the footprint stayed at its peak.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Held while one whole-graph load or build materialises its rows, so two
/// requests never hold two full copies of a large graph at the same time.
pub fn full_graph_guard() -> MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resident bytes one graph row costs while it is materialised: the row as a JSON value, as a
/// typed node or edge, and as the delta that is written back. Measured on the installed Mac
/// build 2411: loading a 1.5M-row graph passed 4.8 GB and was still rising, so 4 KiB is a floor,
/// not a margin.
pub const ESTIMATED_BYTES_PER_ROW: u64 = 4_096;

/// Largest whole graph (nodes + edges) a full build or load may materialise at once. A graph whose
/// stored rows cost more than this is never loaded implicitly; its refresh is deferred and the
/// stored generation kept.
pub const FULL_GRAPH_BYTE_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// Largest set of rows one partial refresh may rebuild: the changed files and the files that
/// reference them. Above it the refresh falls back to a full build when that fits
/// `FULL_GRAPH_BYTE_BUDGET`, and is deferred otherwise.
pub const PARTIAL_REFRESH_BYTE_BUDGET: u64 = 768 * 1024 * 1024;

/// Share of a repository's files a partial refresh may rebuild before it is treated as a large
/// delta (a full build is cheaper than that many partial rewrites).
pub const PARTIAL_REFRESH_MAX_FILE_PERCENT: u64 = 25;

/// A repository with fewer files than this is never sent to a full build by the share rule
/// alone; only the row budget decides (a handful of files is always cheap to rewrite).
pub const PARTIAL_REFRESH_PERCENT_MIN_FILES: usize = 400;

/// Estimated resident bytes of `rows` graph rows.
pub fn estimated_graph_bytes(rows: u64) -> u64 {
    rows.saturating_mul(ESTIMATED_BYTES_PER_ROW)
}

/// Whether a whole graph of `rows` rows fits the full-build budget.
pub fn full_graph_fits(rows: u64) -> bool {
    estimated_graph_bytes(rows) <= FULL_GRAPH_BYTE_BUDGET
}

/// Whether a partial refresh rebuilding `rows` rows fits its budget.
pub fn partial_refresh_fits(rows: u64) -> bool {
    estimated_graph_bytes(rows) <= PARTIAL_REFRESH_BYTE_BUDGET
}

/// Held for a whole incremental refresh, so two repositories never hold two
/// materialised generations at once. Never taken inside `full_graph_guard`.
pub fn refresh_guard() -> MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Return freed heap pages to the OS. Call after a whole-graph load, build or
/// failed build has been dropped; it frees nothing that is still referenced.
pub fn release_unused_memory() {
    platform::release();
}

#[cfg(target_os = "macos")]
mod platform {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
    }
    pub fn release() {
        // SAFETY: a null zone means "all zones"; goal 0 releases as much as possible.
        unsafe {
            malloc_zone_pressure_relief(core::ptr::null_mut(), 0);
        }
    }
}

#[cfg(windows)]
mod platform {
    extern "system" {
        fn GetProcessHeap() -> *mut core::ffi::c_void;
        fn HeapCompact(heap: *mut core::ffi::c_void, flags: u32) -> usize;
    }
    pub fn release() {
        // SAFETY: the process heap handle is always valid; compaction only
        // decommits free blocks.
        unsafe {
            HeapCompact(GetProcessHeap(), 0);
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    pub fn release() {}
}
