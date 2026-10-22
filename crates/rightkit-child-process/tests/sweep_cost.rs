//! The descriptor sweep (Orthic-Labs/rightkit#835) must cost nothing that scales with
//! RLIMIT_NOFILE: the bound is the highest descriptor open in the parent plus a margin, never
//! the limit. With a 65536 soft limit a limit-sized `fcntl` loop in the forked child cost
//! 8-118 ms per spawn on macOS; the bounded loop costs microseconds.
#![cfg(unix)]

use rightkit_child_process::OwnedCommand;
use std::time::Instant;

fn true_binary() -> &'static str {
    ["/usr/bin/true", "/bin/true"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .expect("a `true` executable")
}

fn fastest_micros(runs: u32, mut run: impl FnMut()) -> f64 {
    (0..runs)
        .map(|_| {
            let started = Instant::now();
            run();
            started.elapsed().as_secs_f64() * 1e6
        })
        .fold(f64::MAX, f64::min)
}

#[test]
fn the_sweep_cost_does_not_scale_with_the_descriptor_limit() {
    const RUNS: u32 = 50;
    const ROUNDS: u32 = 3;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let original = limit;
    limit.rlim_cur = 65_536.min(limit.rlim_max);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let bin = true_binary();

    let plain = || {
        let status = std::process::Command::new(bin).status().unwrap();
        assert!(status.success());
    };
    let owned = || {
        let mut child = OwnedCommand::new(bin).spawn().unwrap();
        assert!(child.wait().unwrap().success());
    };
    let (mut base, mut swept) = (f64::MAX, f64::MAX);
    for _ in 0..ROUNDS {
        base = base.min(fastest_micros(RUNS, plain));
        swept = swept.min(fastest_micros(RUNS, owned));
    }
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) };
    let bound = 3_000.0 + 3.0 * base;
    eprintln!("RESULT sweep_cost plain_us={base:.0} owned_us={swept:.0} bound_us={bound:.0}");
    assert!(
        swept - base < bound,
        "owned spawn adds {:.0} us over std ({base:.0} us) at a 65536 soft limit, bound {bound:.0} us",
        swept - base
    );
}
