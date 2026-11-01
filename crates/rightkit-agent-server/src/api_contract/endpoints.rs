// Daemon endpoint manifest, assembled structurally from per-domain group slices.
//
// The old design hard-coded a single `DAEMON_API_ENDPOINT_COUNT` (242) and a
// per-group `copy_endpoint_group(..., offset: usize)`. A drift between the
// constant and any group's literal count left a tail of EMPTY_API_ENDPOINT
// entries in the public `GET /api/contract` array and the OpenAPI document —
// a silent contract regression that the runner would happily ship.
//
// This module derives the count and offsets from the group slices at compile
// time. Adding or removing an endpoint in any group file resizes the manifest
// automatically; there is no separate integer to keep in sync.
//
// Uniqueness across the concatenated manifest is enforced at runtime by a
// focused test (see `engine/bins/coderight/src/api_contract/tests/part_03.rs`)
// because const-fn `&str` equality is not stable on every supported toolchain
// version we ship to. The const-eval path here proves the manifest is well
// sized; the test proves it is well named.

#[path = "endpoints/group_01.rs"]
mod group_01;
#[path = "endpoints/group_02.rs"]
mod group_02;
#[path = "endpoints/group_03.rs"]
mod group_03;
#[path = "endpoints/group_04.rs"]
mod group_04;
#[path = "endpoints/group_05.rs"]
mod group_05;
#[path = "endpoints/group_06.rs"]
mod group_06;
#[path = "endpoints/group_07.rs"]
mod group_07;
#[path = "endpoints/group_08.rs"]
mod group_08;
#[path = "endpoints/group_09.rs"]
mod group_09;
#[path = "endpoints/group_10.rs"]
mod group_10;
#[path = "endpoints/group_11_run_changes.rs"]
mod group_11;
#[path = "endpoints/group_12_desktop.rs"]
mod group_12;
#[path = "endpoints/group_13_drivers.rs"]
mod group_13;
#[path = "endpoints/group_14_inference.rs"]
mod group_14;

const EMPTY_API_ENDPOINT: ApiEndpoint = ApiEndpoint {
    method: "",
    path: "",
    summary: "",
};

/// Compile-time sum of the per-group sizes. Replacing a hand-counted constant
/// with this means an endpoint added to (or removed from) any group file
/// automatically resizes the manifest — there is no separate integer to keep
/// in sync. The sum is a `const` expression, not a `pub` constant, so callers
/// always go through the typed slice below.
const DAEMON_API_ENDPOINT_COUNT: usize = const {
    group_01::ENDPOINT_GROUP_01.len()
        + group_02::ENDPOINT_GROUP_02.len()
        + group_03::ENDPOINT_GROUP_03.len()
        + group_04::ENDPOINT_GROUP_04.len()
        + group_05::ENDPOINT_GROUP_05.len()
        + group_06::ENDPOINT_GROUP_06.len()
        + group_07::ENDPOINT_GROUP_07.len()
        + group_08::ENDPOINT_GROUP_08.len()
        + group_09::ENDPOINT_GROUP_09.len()
        + group_10::ENDPOINT_GROUP_10.len()
        + group_11::ENDPOINT_GROUP_11_RUN_CHANGES.len()
        + group_12::ENDPOINT_GROUP_12_DESKTOP.len()
        + group_12::ENDPOINT_GROUP_12_DESKTOP_TAIL.len()
        + group_13::ENDPOINT_GROUP_13_DRIVERS.len()
        + group_14::ENDPOINT_GROUP_14_INFERENCE.len()
};

/// Endpoints concatenated in group order. Group order is observable from the
/// public contract (the `endpoints` array in `GET /api/contract`); reordering
/// groups is a contract change and must be intentional.
static DAEMON_API_ENDPOINT_ARRAY: [ApiEndpoint; DAEMON_API_ENDPOINT_COUNT] =
    build_daemon_api_endpoints();

pub(crate) const DAEMON_API_ENDPOINTS: &[ApiEndpoint] = &DAEMON_API_ENDPOINT_ARRAY;

const fn copy_endpoint_group<const N: usize, const M: usize>(
    mut endpoints: [ApiEndpoint; N],
    group: [ApiEndpoint; M],
    offset: usize,
) -> [ApiEndpoint; N] {
    let mut index = 0;
    while index < M {
        endpoints[offset + index] = group[index];
        index += 1;
    }
    endpoints
}

const fn build_daemon_api_endpoints() -> [ApiEndpoint; DAEMON_API_ENDPOINT_COUNT] {
    // Offsets are derived from the prefix sums of group sizes. The previous
    // hand-typed offsets (0, 28, 61, 89, 117, 148, 176, 204, 225, 233) drifted
    // when endpoints were added/removed; this sum chain makes every offset a
    // function of the slices above, so the only thing to maintain is the
    // per-group content.
    let endpoints = [EMPTY_API_ENDPOINT; DAEMON_API_ENDPOINT_COUNT];

    let off0 = 0;
    let off1 = off0 + group_01::ENDPOINT_GROUP_01.len();
    let off2 = off1 + group_02::ENDPOINT_GROUP_02.len();
    let off3 = off2 + group_03::ENDPOINT_GROUP_03.len();
    let off4 = off3 + group_04::ENDPOINT_GROUP_04.len();
    let off5 = off4 + group_05::ENDPOINT_GROUP_05.len();
    let off6 = off5 + group_06::ENDPOINT_GROUP_06.len();
    let off7 = off6 + group_07::ENDPOINT_GROUP_07.len();
    let off8 = off7 + group_08::ENDPOINT_GROUP_08.len();
    let off9 = off8 + group_09::ENDPOINT_GROUP_09.len();
    let off10 = off9 + group_10::ENDPOINT_GROUP_10.len();

    let endpoints = copy_endpoint_group(endpoints, group_01::ENDPOINT_GROUP_01, off0);
    let endpoints = copy_endpoint_group(endpoints, group_02::ENDPOINT_GROUP_02, off1);
    let endpoints = copy_endpoint_group(endpoints, group_03::ENDPOINT_GROUP_03, off2);
    let endpoints = copy_endpoint_group(endpoints, group_04::ENDPOINT_GROUP_04, off3);
    let endpoints = copy_endpoint_group(endpoints, group_05::ENDPOINT_GROUP_05, off4);
    let endpoints = copy_endpoint_group(endpoints, group_06::ENDPOINT_GROUP_06, off5);
    let endpoints = copy_endpoint_group(endpoints, group_07::ENDPOINT_GROUP_07, off6);
    let endpoints = copy_endpoint_group(endpoints, group_08::ENDPOINT_GROUP_08, off7);
    let endpoints = copy_endpoint_group(endpoints, group_09::ENDPOINT_GROUP_09, off8);
    let endpoints = copy_endpoint_group(endpoints, group_10::ENDPOINT_GROUP_10, off9);
    let off11 = off10 + group_11::ENDPOINT_GROUP_11_RUN_CHANGES.len();
    let off12 = off11 + group_12::ENDPOINT_GROUP_12_DESKTOP.len();
    let off13 = off12 + group_12::ENDPOINT_GROUP_12_DESKTOP_TAIL.len();
    let endpoints = copy_endpoint_group(endpoints, group_11::ENDPOINT_GROUP_11_RUN_CHANGES, off10);
    let endpoints = copy_endpoint_group(endpoints, group_12::ENDPOINT_GROUP_12_DESKTOP, off11);
    let endpoints = copy_endpoint_group(endpoints, group_12::ENDPOINT_GROUP_12_DESKTOP_TAIL, off12);
    let off14 = off13 + group_13::ENDPOINT_GROUP_13_DRIVERS.len();
    let endpoints = copy_endpoint_group(endpoints, group_13::ENDPOINT_GROUP_13_DRIVERS, off13);
    copy_endpoint_group(endpoints, group_14::ENDPOINT_GROUP_14_INFERENCE, off14)
}
