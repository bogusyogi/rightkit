use rightkit_code_audit_qualification::{assurance, bench};

fn main() {
    let mut failed = false;
    let mut check = |name: &str, ok: bool| {
        if ok {
            println!("ok: {name}");
        } else {
            eprintln!("FAIL: {name}");
            failed = true;
        }
    };
    let digest = assurance::sha256_digest(b"x");
    check("sha256 digest is recognised", assurance::is_digest(&digest));
    check(
        "relative path accepted",
        assurance::safe_relative_path("a/b.txt"),
    );
    check(
        "parent path rejected",
        !assurance::safe_relative_path("../b.txt"),
    );
    check("mean of empty is none", bench::mean(&[]).is_none());
    if failed {
        std::process::exit(1);
    }
}
