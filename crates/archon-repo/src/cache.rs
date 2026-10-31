//! A small, generic disk cache: fingerprint the underlying files/dir once
//! (mtime + size — a handful of `stat()` calls), and skip a full parse
//! entirely if a previous run's cache still matches. Exists because
//! parsing `/var/lib/pacman/sync/*.db` (~15,000 packages, gzip/zstd
//! decompression) and `/var/lib/pacman/local/*/desc` (~1,000 separate
//! file opens — measured slower than the sync db despite far fewer
//! packages, since it's dominated by per-file syscall overhead, not
//! parsing work) took ~175ms and ~750ms respectively on a real system,
//! on *every single command*, even though both only change after
//! `pacman -Sy` or an actual install/remove.
//!
//! Caching here is purely a speed optimization with no correctness
//! weight: any failure (missing file, corrupt JSON, mismatched
//! fingerprint) is treated as a cache miss, never a hard error, and a
//! stale cache at worst shows slightly outdated data for one command
//! before the next fingerprint check catches it — the same staleness
//! window `pacman -Sy`-before-`pacman -S` already has.

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint(Vec<(String, u64, u32, u64, u64, i64, i64)>);

impl Fingerprint {
    /// One entry per file: path, high-resolution timestamps, size and inode. Order
    /// matters (two directories with the same files listed in a
    /// different order are — deliberately — different fingerprints;
    /// callers always pass a consistently-ordered list).
    pub fn of_files(paths: &[PathBuf]) -> std::io::Result<Self> {
        let mut entries = Vec::with_capacity(paths.len());
        for p in paths {
            let meta = std::fs::metadata(p)?;
            entries.push(file_identity(p, &meta));
        }
        Ok(Self(entries))
    }

    /// A directory's own mtime — cheap (one `stat()`) and sufficient for
    /// pacman's local db specifically: installing, removing, or
    /// upgrading a package always adds/removes/renames an immediate
    /// child of `/var/lib/pacman/local`, which always bumps its mtime.
    pub fn of_dir(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self(vec![file_identity(path, &meta)]))
    }
}

fn file_identity(path: &Path, meta: &std::fs::Metadata) -> (String, u64, u32, u64, u64, i64, i64) {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    (
        path.display().to_string(),
        modified.as_secs(),
        modified.subsec_nanos(),
        meta.len(),
        meta.ino(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

#[derive(Serialize, Deserialize)]
struct CacheFile<T> {
    fingerprint: Fingerprint,
    data: T,
}

/// `None` on any miss — wrong/missing file, corrupt content, or a
/// fingerprint mismatch. Never an error; see the module doc comment.
pub fn try_read<T: serde::de::DeserializeOwned>(
    cache_path: &Path,
    current: &Fingerprint,
) -> Option<T> {
    let bytes = std::fs::read(cache_path).ok()?;
    let cached: CacheFile<T> = serde_json::from_slice(&bytes).ok()?;
    (&cached.fingerprint == current).then_some(cached.data)
}

/// Best-effort: write-to-temp-then-rename so a concurrent reader (a
/// second archon invocation racing this one) never observes a
/// partially-written cache file, and any failure along the way (read-only
/// filesystem, disk full, ...) is silently swallowed — a failed cache
/// write must never block the caller, which already has its real answer.
pub fn write<T: Serialize>(cache_path: &Path, fingerprint: Fingerprint, data: &T) {
    let Some(parent) = cache_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(&CacheFile { fingerprint, data }) else {
        return;
    };
    // A unique private file prevents concurrent writers from truncating each
    // other's output or following a planted predictable .tmp symlink.
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if tmp.write_all(&bytes).is_ok() {
        let _ = tmp.persist(cache_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.json");
        let fp = Fingerprint(vec![("a".into(), 1, 0, 2, 3, 4, 0)]);
        write(&cache_path, fp.clone(), &vec![1, 2, 3]);
        let got: Option<Vec<i32>> = try_read(&cache_path, &fp);
        assert_eq!(got, Some(vec![1, 2, 3]));
    }

    #[test]
    fn mismatched_fingerprint_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.json");
        write(
            &cache_path,
            Fingerprint(vec![("a".into(), 1, 0, 2, 3, 4, 0)]),
            &vec![1, 2, 3],
        );
        let got: Option<Vec<i32>> = try_read(
            &cache_path,
            &Fingerprint(vec![("a".into(), 9, 0, 9, 3, 4, 0)]),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn missing_cache_file_is_a_miss_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("does-not-exist.json");
        let got: Option<Vec<i32>> = try_read(&cache_path, &Fingerprint(vec![]));
        assert_eq!(got, None);
    }

    #[test]
    fn corrupt_cache_file_is_a_miss_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.json");
        std::fs::write(&cache_path, b"not json").unwrap();
        let got: Option<Vec<i32>> = try_read(&cache_path, &Fingerprint(vec![]));
        assert_eq!(got, None);
    }
    #[test]
    fn same_size_replacement_and_subsecond_changes_invalidate_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        std::fs::write(&path, "one").unwrap();
        let before = Fingerprint::of_files(std::slice::from_ref(&path)).unwrap();
        let replacement = dir.path().join("new-db");
        std::fs::write(&replacement, "two").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(
            before,
            Fingerprint::of_files(std::slice::from_ref(&path)).unwrap()
        );
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let timestamp = UNIX_EPOCH + std::time::Duration::new(100, 1);
        file.set_modified(timestamp).unwrap();
        let first = Fingerprint::of_files(std::slice::from_ref(&path)).unwrap();
        file.set_modified(timestamp + std::time::Duration::from_nanos(1))
            .unwrap();
        assert_ne!(first, Fingerprint::of_files(&[path]).unwrap());
    }

    #[test]
    fn concurrent_writes_publish_complete_snapshots_without_following_tmp_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let victim = dir.path().join("keep");
        std::fs::write(&victim, "untouched").unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("tmp")).unwrap();
        let fp = Fingerprint(vec![]);
        std::thread::scope(|scope| {
            for n in 0..8 {
                let path = &path;
                let fp = &fp;
                scope.spawn(move || {
                    for _ in 0..10 {
                        write(path, fp.clone(), &vec![n; 100]);
                    }
                });
            }
        });
        let snapshot: Vec<usize> = try_read(&path, &fp).unwrap();
        assert_eq!(snapshot.len(), 100);
        assert!(snapshot.iter().all(|n| *n == snapshot[0]));
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "untouched");
    }
}
