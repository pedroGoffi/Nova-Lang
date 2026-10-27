//! Editor-only per-process source snapshot overlay.
//! A private manifest maps real absolute source paths to temporary buffer copies.
//! The resolver retains original paths for package/module visibility and diagnostic
//! filenames, avoiding edits to the working tree or Nova.lock.
use std::{
    collections::HashMap,
    env, fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static OVERLAYS: OnceLock<HashMap<PathBuf, PathBuf>> = OnceLock::new();

/// Canonicalize existing files; for not-yet-created files, canonicalize the
/// existing parent. This allows an unsaved `mod foo;` target in the editor.
pub fn normalized_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    if let Some(parent) = path.parent() {
        // Directory of a new module may not exist, but it is safe to reject
        // it until created, rather than creating project directories in LSP.
        let directory = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        return Ok(directory
            .canonicalize()?
            .join(path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing filename")
            })?));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "missing parent",
    ))
}

fn read_manifest(manifest: &Path) -> HashMap<PathBuf, PathBuf> {
    let mut map = HashMap::new();
    let Ok(data) = fs::read_to_string(manifest) else {
        return map;
    };
    for record in data.lines() {
        let Some((original, replacement)) = record.split_once('\t') else {
            continue;
        };
        let (original, replacement) = (PathBuf::from(original), PathBuf::from(replacement));
        if !original.is_absolute() || !replacement.is_absolute() {
            continue;
        }
        if let Ok(path) = normalized_path(&original) {
            // Never modify a project file. A missing temporary file is not an overlay.
            if replacement.is_file() {
                map.insert(path, replacement);
            }
        }
    }
    map
}
fn mapping() -> &'static HashMap<PathBuf, PathBuf> {
    OVERLAYS.get_or_init(|| {
        env::var_os("NOVA_LSP_OVERLAY_MAP")
            .map(|path| read_manifest(Path::new(&path)))
            .unwrap_or_default()
    })
}

pub fn source_path(original: &Path) -> PathBuf {
    let key = normalized_path(original).unwrap_or_else(|_| original.to_path_buf());
    mapping().get(&key).cloned().unwrap_or(key)
}
pub fn exists(original: &Path) -> bool {
    original.is_file()
        || normalized_path(original)
            .ok()
            .is_some_and(|key| mapping().contains_key(&key))
}
pub fn read_to_string(original: &Path) -> io::Result<String> {
    fs::read_to_string(source_path(original))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    #[test]
    fn manifest_normalizes_existing_and_new_sources_without_touching_original() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("nova-overlay-test-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("old.nova");
        let fresh = dir.join("new.nova");
        let scratch = dir.join("scratch.nova");
        let manifest = dir.join("overlay.map");
        fs::write(&existing, "original").unwrap();
        fs::write(&scratch, "editor buffer").unwrap();
        fs::write(
            &manifest,
            format!(
                "{}\t{}\n{}\t{}\n",
                existing.display(),
                scratch.display(),
                fresh.display(),
                scratch.display()
            ),
        )
        .unwrap();
        let map = read_manifest(&manifest);
        assert_eq!(map.get(&existing), Some(&scratch));
        assert_eq!(map.get(&fresh), Some(&scratch));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "original");
        assert!(!fresh.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
