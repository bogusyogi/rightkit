// Reads a selector only when the capability is compiled in.
fn main() {
    #[cfg(feature = "test-mode")]
    {
        let _root = std::env::var("FIXTURE_QA_ROOT");
    }
    println!("fixture");
}
