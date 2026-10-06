//! In-process ZIP writer (HeardRight `diagnostics_archive.rs`); never launches
//! platform archive tools.

use std::fs;
use std::path::Path;

/// ZIP `source` (a directory) to `destination`, rooted at the directory's name.
pub fn create_zip(source: &Path, destination: &Path) -> Result<(), String> {
    let root_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "staging directory has no portable name".to_string())?;
    let file = fs::File::create(destination).map_err(|e| format!("create ZIP: {e}"))?;
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    archive
        .add_directory(format!("{root_name}/"), options)
        .map_err(|e| format!("add ZIP root: {e}"))?;
    add_tree(&mut archive, source, source, root_name, options)?;
    archive.finish().map_err(|e| format!("finish ZIP: {e}"))?;
    Ok(())
}

fn add_tree(
    archive: &mut zip::ZipWriter<fs::File>,
    root: &Path,
    current: &Path,
    root_name: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    let mut entries = fs::read_dir(current)
        .map_err(|e| format!("read ZIP source: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read ZIP entry: {e}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| format!("resolve ZIP path: {e}"))?;
        let name = format!(
            "{root_name}/{}",
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let file_type = entry
            .file_type()
            .map_err(|e| format!("inspect ZIP entry: {e}"))?;
        if file_type.is_dir() {
            archive
                .add_directory(format!("{name}/"), options)
                .map_err(|e| format!("add ZIP directory: {e}"))?;
            add_tree(archive, root, &path, root_name, options)?;
        } else if file_type.is_file() {
            archive
                .start_file(name, options)
                .map_err(|e| format!("add ZIP file: {e}"))?;
            let mut input = fs::File::open(&path).map_err(|e| format!("open ZIP input: {e}"))?;
            std::io::copy(&mut input, archive).map_err(|e| format!("write ZIP file: {e}"))?;
        }
        // Symlinks and special files are skipped on purpose.
    }
    Ok(())
}
