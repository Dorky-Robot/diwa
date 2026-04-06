//! Track which repos diwa is installed in.
//!
//! Stores a manifest at ~/.diwa/repos.json mapping slugs to local paths.
//! Used by `diwa update` to find and refresh all managed repos.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = "repos.json";
const INDEX_DB_FILE: &str = "index.db";

/// Read the manifest of installed repos.
pub fn read_manifest(diwa_dir: &Path) -> HashMap<String, PathBuf> {
    let path = diwa_dir.join(MANIFEST_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Add a repo to the manifest.
pub fn register_repo(diwa_dir: &Path, slug: &str, local_path: &Path) -> Result<()> {
    let mut manifest = read_manifest(diwa_dir);
    manifest.insert(slug.to_string(), local_path.to_path_buf());
    write_manifest(diwa_dir, &manifest)
}

/// Remove a repo from the manifest.
pub fn unregister_repo(diwa_dir: &Path, slug: &str) -> Result<()> {
    let mut manifest = read_manifest(diwa_dir);
    manifest.remove(slug);
    write_manifest(diwa_dir, &manifest)
}

/// Scan `diwa_dir` for subdirectories that contain an `index.db`,
/// returning the slug (directory name) for each.
///
/// This is the source of truth for "what repos actually have an index on disk",
/// independent of whether they were ever registered in the manifest. Used by
/// `diwa ls` so that a repo indexed by the post-commit hook still shows up even
/// when the manifest is missing or stale.
pub fn scan_indexed_slugs(diwa_dir: &Path) -> Vec<String> {
    let mut slugs = Vec::new();
    let Ok(entries) = std::fs::read_dir(diwa_dir) else {
        return slugs;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if !entry.path().join(INDEX_DB_FILE).is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        slugs.push(name);
    }
    slugs.sort();
    slugs
}

fn write_manifest(diwa_dir: &Path, manifest: &HashMap<String, PathBuf>) -> Result<()> {
    let path = diwa_dir.join(MANIFEST_FILE);
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_read_empty_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest = read_manifest(tmp.path());
        assert!(manifest.is_empty());
    }

    #[test]
    fn test_register_and_read() {
        let tmp = TempDir::new().unwrap();
        register_repo(tmp.path(), "owner--repo", Path::new("/path/to/repo")).unwrap();
        let manifest = read_manifest(tmp.path());
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest["owner--repo"], PathBuf::from("/path/to/repo"));
    }

    #[test]
    fn test_unregister() {
        let tmp = TempDir::new().unwrap();
        register_repo(tmp.path(), "a--b", Path::new("/a")).unwrap();
        register_repo(tmp.path(), "c--d", Path::new("/c")).unwrap();
        unregister_repo(tmp.path(), "a--b").unwrap();
        let manifest = read_manifest(tmp.path());
        assert_eq!(manifest.len(), 1);
        assert!(!manifest.contains_key("a--b"));
    }

    // ---- scan_indexed_slugs ----

    #[test]
    fn test_scan_indexed_slugs_empty_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(scan_indexed_slugs(tmp.path()).is_empty());
    }

    #[test]
    fn test_scan_indexed_slugs_missing_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(scan_indexed_slugs(&missing).is_empty());
    }

    #[test]
    fn test_scan_indexed_slugs_finds_dirs_with_index_db() {
        let tmp = TempDir::new().unwrap();
        // two slugs with index.db, one without
        for slug in ["owner--one", "owner--two"] {
            let dir = tmp.path().join(slug);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("index.db"), b"stub").unwrap();
        }
        let no_db = tmp.path().join("owner--three");
        std::fs::create_dir_all(&no_db).unwrap();
        // a stray file at the top level must not confuse us
        std::fs::write(tmp.path().join("repos.json"), b"{}").unwrap();

        let found = scan_indexed_slugs(tmp.path());
        assert_eq!(found, vec!["owner--one".to_string(), "owner--two".to_string()]);
    }

    #[test]
    fn test_scan_indexed_slugs_ignores_index_db_files_at_top_level() {
        let tmp = TempDir::new().unwrap();
        // index.db directly under diwa_dir (not inside a slug subdir) should be ignored
        std::fs::write(tmp.path().join("index.db"), b"stub").unwrap();
        assert!(scan_indexed_slugs(tmp.path()).is_empty());
    }
}
