// rightkit-gpu 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// No adapter is enumerated and no device is created: `GpuContext::create` and `adapter_names` load GPU
// drivers, so a consumer lane without a GPU must not call them. This consumer exercises the backend
// policy, limits parsing, adapter pick, the memory budget and the exit gate, all pure or in-process.
// No init marker is set, so nothing is written to disk, and the process-wide shutdown is never begun.
use std::time::Duration;

use rightkit_gpu::backend::{
    choose, default_compute_backends, default_window_backends, parse_backends, read_init_marker,
    resolve, with_init_marker, BackendChoice, Backends,
};
use rightkit_gpu::exit::{self, GpuGate};
use rightkit_gpu::{
    footprint, initial, parse_limits_cap, pick_adapter, ContextOptions, LimitsCap, MemoryBudget,
};

const MIB: u64 = 1 << 20;

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Backend list parsing.
    check(
        "auto",
        parse_backends(" Auto ") == Some(BackendChoice::Auto),
    );
    check(
        "off spellings",
        parse_backends("off") == Some(BackendChoice::Off)
            && parse_backends("cpu") == Some(BackendChoice::Off),
    );
    check(
        "glow keeps the compute default",
        parse_backends("glow") == Some(BackendChoice::Auto),
    );
    check("empty names nothing", parse_backends("").is_none());
    check(
        "unknown names nothing",
        parse_backends("not-a-backend").is_none(),
    );
    check(
        "dx12",
        parse_backends("dx12") == Some(BackendChoice::Use(Backends::DX12)),
    );

    // The app's variable wins; WGPU_BACKEND selects backends but never turns the GPU off.
    check(
        "app variable off",
        choose("APP_GPU", Some("off"), None) == BackendChoice::Off,
    );
    check(
        "WGPU_BACKEND cannot switch the GPU off",
        choose("APP_GPU", None, Some("off")) == BackendChoice::Auto,
    );
    check(
        "unknown app value falls through to WGPU_BACKEND",
        choose("APP_GPU", Some("bogus"), Some("dx12")) == BackendChoice::Use(Backends::DX12),
    );
    check(
        "nothing asked is auto",
        choose("APP_GPU", None, None) == BackendChoice::Auto,
    );

    // Platform defaults and resolution against what the build contains.
    check(
        "per-OS compute defaults",
        default_compute_backends("windows") == Backends::DX12
            && default_compute_backends("macos") == Backends::METAL
            && default_compute_backends("linux") == Backends::PRIMARY,
    );
    check(
        "per-OS window defaults",
        default_window_backends("windows") == Backends::DX12
            && default_window_backends("macos") == Backends::METAL
            && default_window_backends("linux") == (Backends::PRIMARY | Backends::GL),
    );
    let compiled = Backends::VULKAN | Backends::METAL;
    check(
        "off resolves to no GPU",
        resolve(BackendChoice::Off, Backends::METAL, compiled).is_none(),
    );
    check(
        "auto resolves to the default",
        resolve(BackendChoice::Auto, Backends::METAL, compiled) == Some(Backends::METAL),
    );
    check(
        "a compiled-in choice is kept",
        resolve(
            BackendChoice::Use(Backends::VULKAN),
            Backends::METAL,
            compiled,
        ) == Some(Backends::VULKAN),
    );
    check(
        "a backend missing from the build falls back to the default",
        resolve(
            BackendChoice::Use(Backends::DX12),
            Backends::METAL,
            compiled,
        ) == Some(Backends::METAL),
    );

    // Limits cap and adapter pick.
    check(
        "numeric cap",
        parse_limits_cap("128")
            == Some(LimitsCap {
                storage_binding_bytes: 128 * MIB,
                buffer_bytes: 256 * MIB,
            }),
    );
    check(
        "cap with a unit",
        parse_limits_cap("64MiB")
            == Some(LimitsCap {
                storage_binding_bytes: 64 * MIB,
                buffer_bytes: 128 * MIB,
            }),
    );
    check(
        "zero cap is one MiB",
        parse_limits_cap("0").map(|cap| cap.storage_binding_bytes) == Some(MIB),
    );
    check(
        "webgpu cap",
        parse_limits_cap("webgpu").is_some_and(|cap| {
            cap.buffer_bytes >= cap.storage_binding_bytes && cap.storage_binding_bytes > 0
        }),
    );
    check(
        "empty and unparsable caps",
        parse_limits_cap("").is_none() && parse_limits_cap("lots").is_none(),
    );
    let names = vec!["Integrated".to_string(), "Discrete".to_string()];
    check(
        "adapter pick",
        pick_adapter(&names, "Discrete") == Some(1) && pick_adapter(&names, "Missing").is_none(),
    );
    let options = ContextOptions::new(Backends::METAL, "consumer");
    check(
        "context option defaults",
        options.backends == Backends::METAL
            && options.adapter_name.is_none()
            && !options.low_power
            && options.min_storage_buffers_per_stage == 8
            && options.limits_cap.is_none()
            && options.label == "consumer",
    );

    // Memory budget sizing and admission.
    check("footprint", footprint(1920, 1080) == 1920 * 1080 * 64);
    check(
        "initial budget",
        initial(0) == 1 << 30 && initial(1 << 30) == (1 << 29) * 3,
    );
    let budget = MemoryBudget::new(1 << 30);
    {
        let first = budget.admit(300 * MIB, false, || false);
        let second = budget.admit(300 * MIB, false, || true);
        check(
            "two renders fit beside each other",
            first.is_some() && second.is_some(),
        );
    }
    budget.out_of_memory();
    {
        let first = budget.admit(300 * MIB, false, || false);
        let second = budget.admit(300 * MIB, false, || true);
        check(
            "after out-of-memory the budget is halved",
            first.is_some() && second.is_none(),
        );
        let exclusive = budget.admit(MIB, true, || true);
        check(
            "an exclusive retry waits for the device",
            exclusive.is_none(),
        );
    }
    budget.succeeded();
    budget.succeeded();
    {
        let first = budget.admit(300 * MIB, false, || false);
        let second = budget.admit(300 * MIB, false, || true);
        check(
            "successes grow the budget back",
            first.is_some() && second.is_some(),
        );
    }

    // A private exit gate: work in flight holds it busy; closing refuses new work.
    let gate = GpuGate::new();
    let work = gate.enter();
    check("work enters an open gate", work.is_some());
    check(
        "a busy gate is not idle",
        !gate.wait_idle(Duration::from_millis(20)),
    );
    drop(work);
    check(
        "an idle gate reports idle",
        gate.wait_idle(Duration::from_millis(20)),
    );
    gate.close();
    check("a closed gate refuses work", gate.enter().is_none());
    // The process-wide gate is only read and entered, never closed.
    check("the process is not shutting down", !exit::shutting_down());
    check("process-wide work enters", exit::enter().is_some());

    // The init marker: with none set, driver work just runs, and a missing marker reads as absent.
    check(
        "driver work runs without a marker",
        with_init_marker(Backends::METAL, || 42) == 42,
    );
    let missing =
        std::env::temp_dir().join(format!("rightkit-gpu-consumer-{}", std::process::id()));
    check(
        "a missing marker reads as absent",
        read_init_marker(&missing).is_none() && !missing.exists(),
    );

    println!("OK rightkit-gpu 0.1.0 (registry consumer, public API)");
}
