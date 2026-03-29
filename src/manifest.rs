//! Track which repos diwa is installed in.
//!
//! Stores a manifest at ~/.diwa/repos.json mapping slugs to local paths.
//! Used by `diwa update` to find and refresh all managed repos.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = "repos.json";

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
}
