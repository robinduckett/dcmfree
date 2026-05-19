//! Layer enumeration and classification.
//!
//! A "layer" is a direct child of the HCS Layers directory. Each one is
//! self-contained (image files + metadata + bindings). We compute size and
//! age non-recursively in the public summary, and only walk recursively for
//! the byte total.

use crate::errors::DcmFreeError;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

/// Per-layer summary derived purely from the filesystem.
#[derive(Debug, Clone, Serialize)]
pub struct LayerInfo {
    pub path: PathBuf,
    pub id: String,
    pub size_bytes: u64,
    pub file_count: u64,
    /// Creation time of the layer's top-level directory (NTFS `CreationTime`).
    /// This never changes once the layer exists and is therefore the most
    /// reliable measure of how old the layer is.
    pub created_at: SystemTime,
    /// Most recent mtime found inside this layer. Can be affected by tools
    /// that touched files inside the layer (including failed delete attempts).
    pub last_modified: SystemTime,
    /// Most recent atime on the layer's top-level directory. Often equal to
    /// `created_at` because Windows disables last-access updates by default.
    pub last_accessed: SystemTime,
    /// True when no live container/image references this layer.
    pub orphan: bool,
}

impl LayerInfo {
    /// Age of the layer relative to `now`, derived from `created_at`.
    /// Saturates at zero if the timestamp is in the future.
    #[must_use]
    pub fn age(&self, now: SystemTime) -> Duration {
        now.duration_since(self.created_at).unwrap_or_default()
    }
}

/// Enumerate all layer subdirectories at `dir`. Hidden, non-directory, and
/// system entries are skipped; permission errors on individual files do not
/// fail the enumeration.
pub fn enumerate(dir: &Path) -> Result<Vec<LayerInfo>, DcmFreeError> {
    if !dir.exists() {
        return Err(DcmFreeError::LayersDirMissing(dir.to_path_buf()));
    }

    let entries = std::fs::read_dir(dir).map_err(|e| DcmFreeError::io(dir.to_path_buf(), e))?;

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        out.push(stat_layer(path, id));
    }
    out.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Ok(out)
}

/// Build a `LayerInfo` for a single layer directory.
///
/// Recursive walk for size + file count + max(mtime). Top-level directory
/// metadata for `created_at` and `last_accessed` — those are the reliable
/// timestamps (created never changes; atime usually equals created on
/// Windows because last-access updates are disabled by default).
///
/// Tolerant of individual entry failures: reparse points and access-denied
/// files inside HCS layers are common, and they just don't contribute to
/// the size/count totals.
#[must_use]
pub fn stat_layer(path: PathBuf, id: String) -> LayerInfo {
    let dir_md = std::fs::metadata(&path).ok();
    let created_at = dir_md
        .as_ref()
        .and_then(|m| m.created().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let last_accessed = dir_md
        .as_ref()
        .and_then(|m| m.accessed().ok())
        .unwrap_or(created_at);
    let mut last_modified = dir_md.and_then(|m| m.modified().ok()).unwrap_or(created_at);

    let mut size: u64 = 0;
    let mut file_count: u64 = 0;
    for entry in WalkDir::new(&path).into_iter().flatten() {
        let Ok(md) = entry.metadata() else { continue };
        if md.is_file() {
            size = size.saturating_add(md.len());
            file_count = file_count.saturating_add(1);
        }
        if let Ok(m) = md.modified()
            && m > last_modified
        {
            last_modified = m;
        }
    }

    LayerInfo {
        path,
        id,
        size_bytes: size,
        file_count,
        created_at,
        last_modified,
        last_accessed,
        orphan: true, // tentative; classifier will refine
    }
}

/// Mark every layer whose path appears in `in_use` as non-orphan.
pub fn classify<S: std::hash::BuildHasher>(
    layers: &mut [LayerInfo],
    in_use: &std::collections::HashSet<PathBuf, S>,
) {
    for l in layers.iter_mut() {
        // Compare via canonicalized path when possible — Docker reports
        // case-insensitive paths on Windows and may use \\?\ prefixes.
        let canonical = std::fs::canonicalize(&l.path).unwrap_or_else(|_| l.path.clone());
        l.orphan = !in_use.iter().any(|p| paths_equal(&canonical, p));
    }
}

/// Case-insensitive path equality, normalising the `\\?\` prefix Windows
/// sometimes returns from canonicalize.
fn paths_equal(a: &Path, b: &Path) -> bool {
    let a_s = strip_verbatim_prefix(a);
    let b_s = strip_verbatim_prefix(b);
    a_s.eq_ignore_ascii_case(&b_s)
}

fn strip_verbatim_prefix(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Filter a slice of `LayerInfo` to the orphan subset older than `min_age`.
#[must_use]
pub fn orphans_older_than(
    layers: &[LayerInfo],
    min_age: Duration,
    now: SystemTime,
) -> Vec<&LayerInfo> {
    layers
        .iter()
        .filter(|l| l.orphan && l.age(now) >= min_age)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn enumerate_empty_dir() {
        let dir = tempdir().unwrap();
        let layers = enumerate(dir.path()).unwrap();
        assert!(layers.is_empty());
    }

    #[test]
    fn enumerate_missing_dir_errors() {
        let err = enumerate(Path::new(r"C:\definitely-not-a-dir-xyz")).unwrap_err();
        assert!(matches!(err, DcmFreeError::LayersDirMissing(_)));
    }

    #[test]
    fn enumerate_counts_subdirs_only() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("layer-a")).unwrap();
        fs::create_dir(dir.path().join("layer-b")).unwrap();
        fs::write(dir.path().join("loose-file"), b"x").unwrap();
        let layers = enumerate(dir.path()).unwrap();
        assert_eq!(layers.len(), 2);
        let ids: Vec<_> = layers.iter().map(|l| l.id.as_str()).collect();
        assert!(ids.contains(&"layer-a"));
        assert!(ids.contains(&"layer-b"));
    }

    #[test]
    fn enumerate_computes_sizes_and_sorts_descending() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("small")).unwrap();
        fs::write(dir.path().join("small").join("f"), vec![0u8; 100]).unwrap();
        fs::create_dir(dir.path().join("big")).unwrap();
        fs::write(dir.path().join("big").join("f"), vec![0u8; 10_000]).unwrap();
        let layers = enumerate(dir.path()).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].id, "big");
        assert!(layers[0].size_bytes >= 10_000);
        assert!(layers[1].size_bytes < layers[0].size_bytes);
    }

    #[test]
    fn classify_marks_known_as_non_orphan() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("layer-a")).unwrap();
        fs::create_dir(dir.path().join("layer-b")).unwrap();
        let mut layers = enumerate(dir.path()).unwrap();
        let mut in_use = std::collections::HashSet::new();
        let canon_a = std::fs::canonicalize(dir.path().join("layer-a")).unwrap();
        in_use.insert(canon_a);
        classify(&mut layers, &in_use);
        let by_id: std::collections::HashMap<_, _> =
            layers.iter().map(|l| (l.id.as_str(), l.orphan)).collect();
        assert!(!by_id["layer-a"]);
        assert!(by_id["layer-b"]);
    }

    fn fake_layer(id: &str, age_secs: u64, orphan: bool) -> LayerInfo {
        let now = SystemTime::now();
        LayerInfo {
            path: PathBuf::from(id),
            id: id.into(),
            size_bytes: 10,
            file_count: 1,
            created_at: now - Duration::from_secs(age_secs),
            last_modified: now - Duration::from_secs(age_secs),
            last_accessed: now - Duration::from_secs(age_secs),
            orphan,
        }
    }

    #[test]
    fn orphans_older_than_filters_by_age_and_orphan_status() {
        let now = SystemTime::now();
        let layers = vec![
            fake_layer("a", 10 * 86_400, true),
            fake_layer("b", 60, true),
            fake_layer("c", 10 * 86_400, false),
        ];
        let picked = orphans_older_than(&layers, Duration::from_secs(86_400), now);
        let ids: Vec<&str> = picked.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn age_saturates_for_future_creation_time() {
        let now = SystemTime::now();
        let l = LayerInfo {
            path: PathBuf::from("a"),
            id: "a".into(),
            size_bytes: 0,
            file_count: 0,
            created_at: now + Duration::from_secs(3600),
            last_modified: now,
            last_accessed: now,
            orphan: true,
        };
        assert_eq!(l.age(now), Duration::ZERO);
    }

    #[test]
    fn paths_equal_handles_verbatim_prefix() {
        assert!(paths_equal(
            Path::new(r"\\?\C:\foo\bar"),
            Path::new(r"C:\FOO\BAR"),
        ));
    }
}
