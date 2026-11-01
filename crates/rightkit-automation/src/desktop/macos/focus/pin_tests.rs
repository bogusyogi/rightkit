use super::*;

#[test]
fn app_identity_miss_keeps_attempted_pid_and_broad_compatibility_label() {
    let miss = FocusPinMiss::NoAppIdentity {
        attempted_pid: 2359,
        cause: RunningApplicationIdentityMiss::NotFound,
    };
    assert_eq!(miss.label(), "no_app_identity");
    assert_eq!(miss.detail_label(), "running_app_not_found");
    assert_eq!(miss.attempted_pid(), Some(2359));
}

#[test]
fn process_instance_kernel_validation_fails_closed_without_content() {
    assert_eq!(
        RunningProcessInstanceMiss::KernelInvalidStartMicroseconds.label(),
        "kernel_invalid_start_microseconds"
    );
    let mut info = unsafe { std::mem::zeroed::<ProcBsdInfo>() };
    info.pbi_pid = 2359;
    info.pbi_start_tvsec = 1;
    info.pbi_start_tvusec = 1_000_000;
    assert_eq!(
        process_instance_from_kernel_info(2359, &info),
        Err(RunningProcessInstanceMiss::KernelInvalidStartMicroseconds)
    );
    info.pbi_start_tvusec = 1;
    assert_eq!(
        process_instance_from_kernel_info(2358, &info),
        Err(RunningProcessInstanceMiss::KernelPidMismatch)
    );
    info.pbi_pid = 2358;
    info.pbi_start_tvsec = 0;
    assert_eq!(
        process_instance_from_kernel_info(2358, &info),
        Err(RunningProcessInstanceMiss::KernelZeroStartSeconds)
    );
    info.pbi_pid = 2359;
    info.pbi_start_tvsec = 17;
    assert_eq!(
        process_instance_from_kernel_info(2359, &info),
        Ok(17_000_001_000)
    );
    info.pbi_start_tvsec = u64::MAX;
    assert_eq!(
        process_instance_from_kernel_info(2359, &info),
        Err(RunningProcessInstanceMiss::KernelStartTimeOverflow)
    );
}

#[test]
fn kernel_process_instance_is_stable_for_current_process() {
    let pid = std::process::id() as i32;
    let first = running_process_instance_detailed(pid).expect("current process start time");
    let second = running_process_instance_detailed(pid).expect("current process start time");
    assert!(first > 0);
    assert_eq!(first, second);
}
