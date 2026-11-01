//! The planted-secret bench case is stored as a template so that no file of the crate holds a
//! secret-shaped literal. This harness materialises the real file into a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};

const PLACEHOLDER: &str = "@AWS_KEY@";

/// An AWS-access-key-id-shaped value, assembled at run time.
fn aws_key() -> String {
    format!("{}{}", concat!("AK", "IA"), "XQ7PLM3NB2ZRT5HY")
}

/// Copy `fixtures/bench/<case>` into `dest`, expanding every `*.tmpl` file (placeholder replaced,
/// suffix dropped) and copying other files verbatim.
fn materialise(case: &str, dest: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bench")
        .join(case);
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(&source).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match name.strip_suffix(".tmpl") {
            Some(real) => {
                let text = fs::read_to_string(&path).unwrap();
                fs::write(dest.join(real), text.replace(PLACEHOLDER, &aws_key())).unwrap();
            }
            None => {
                fs::copy(&path, dest.join(&name)).unwrap();
            }
        }
    }
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-bench-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn planted_secret_case_materialises_a_key_shaped_literal() {
    let dir = scratch("001");
    materialise("001-hardcoded-secret", &dir);
    let text = fs::read_to_string(dir.join("app.js")).unwrap();
    assert!(!text.contains(PLACEHOLDER));
    assert!(text.contains(&format!("\"{}\"", aws_key())));
    assert_eq!(aws_key().len(), 20);
    let _ = fs::remove_dir_all(&dir);
}
