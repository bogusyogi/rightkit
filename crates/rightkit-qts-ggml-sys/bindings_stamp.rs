// Shared by build.rs (`include!`) and tests/bindings_match.rs (`#[path]`).
// Keep this file std-only and free of crate imports.

/// Version string from `set(GGML_VERSION_{MAJOR,MINOR,PATCH} N)` lines in ggml's CMakeLists.txt.
pub fn ggml_version_from_cmake(cmake: &str) -> Option<String> {
    let part = |name: &str| -> Option<String> {
        let prefix = format!("set(GGML_VERSION_{name} ");
        cmake.lines().find_map(|line| {
            line.trim()
                .strip_prefix(prefix.as_str())
                .map(|rest| rest.trim_end_matches(')').trim().to_string())
        })
    };
    Some(format!(
        "{}.{}.{}",
        part("MAJOR")?,
        part("MINOR")?,
        part("PATCH")?
    ))
}

/// FNV-1a 64-bit digest over every public ggml header (`include/*.h`), in name order.
/// Any header edit changes the digest, so stale bindings fail the match test.
pub fn ggml_headers_digest(include_dir: &std::path::Path) -> std::io::Result<String> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(include_dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "h"))
        .collect();
    paths.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for path in &paths {
        feed(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
        feed(&[0]);
        feed(&std::fs::read(path)?);
        feed(&[0]);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

/// The single stamp line written at the top of every regenerated bindings file.
#[allow(dead_code)]
pub fn stamp_line(ggml: &str, headers: &str, target: &str) -> String {
    format!("// ggml-sys-stamp ggml={ggml} headers={headers} target={target}")
}

/// Parsed `ggml-sys-stamp` line.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub struct Stamp {
    pub ggml: String,
    pub headers: String,
    pub target: String,
}

/// Reads the `// ggml-sys-stamp ...` line from a bindings file, if present.
#[allow(dead_code)]
pub fn parse_stamp(contents: &str) -> Option<Stamp> {
    let line = contents
        .lines()
        .find(|line| line.starts_with("// ggml-sys-stamp "))?;
    let (mut ggml, mut headers, mut target) = (None, None, None);
    for field in line.split_whitespace().skip(2) {
        match field.split_once('=') {
            Some(("ggml", value)) => ggml = Some(value.to_string()),
            Some(("headers", value)) => headers = Some(value.to_string()),
            Some(("target", value)) => target = Some(value.to_string()),
            _ => {}
        }
    }
    Some(Stamp {
        ggml: ggml?,
        headers: headers?,
        target: target?,
    })
}
