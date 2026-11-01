#[cfg(test)]
mod tests {
    /// Rich file reads (image/notebook/PDF) dispatch through the isolated
    /// `rich_parser_worker` process, which enforces sandbox confinement
    /// (CG-B02) by default. These tests exercise parsing/format logic, not
    /// confinement itself, so they swap in the unconfined test-double
    /// launch adapter for their duration — a confined child cannot always
    /// reach a plain `tempdir()` fixture on every platform. The guard
    /// resets the override on drop so it never leaks into other tests.
    struct UnconfinedRichWorkerGuard;

    impl Drop for UnconfinedRichWorkerGuard {
        fn drop(&mut self) {
            crate::coding_tools::rich_parser_worker::set_test_launch_adapter(None);
        }
    }

    fn unconfined_rich_worker() -> UnconfinedRichWorkerGuard {
        crate::coding_tools::rich_parser_worker::set_test_launch_adapter(Some(std::sync::Arc::new(
            crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
        )));
        UnconfinedRichWorkerGuard
    }

    include!("tests_part1.rs");
    include!("tests_part2.rs");
}
