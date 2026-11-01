pub(crate) fn plugin_source_sha256(source_path: &FsPath) -> Result<String, String> {
    let source = source_path
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize plugin source: {e}"))?;
    if !source.is_dir() {
        return Err("plugin source is not a directory".to_string());
    }
    let mut files = Vec::new();
    collect_plugin_source_files(&source, &mut files)?;
    files.sort();

    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for file in files {
        let relative = file
            .strip_prefix(&source)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        hasher.update(&[0xff]);
    }
    Ok(hasher.finalize())
}

fn collect_plugin_source_files(dir: &FsPath, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            collect_plugin_source_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

pub(crate) fn unpack_plugin_package(bytes: &[u8], destination: &FsPath) -> Result<PathBuf, String> {
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        entry.unpack_in(destination).map_err(|e| e.to_string())?;
    }
    find_packaged_plugin_root(destination)
}

fn find_packaged_plugin_root(destination: &FsPath) -> Result<PathBuf, String> {
    if destination.join("plugin.json").exists() {
        return Ok(destination.to_path_buf());
    }
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(destination).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() && path.join("plugin.json").exists() {
            candidates.push(path);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err("plugin package did not contain plugin.json".into()),
        _ => Err("plugin package contained multiple plugin roots".into()),
    }
}
