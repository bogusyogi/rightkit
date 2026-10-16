#[test]
fn adopted_process_is_observation_only_at_compile_time() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/adopted_cannot_terminate.rs");
}
