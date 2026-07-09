use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use archon_types::PackageMeta;

use crate::desc::{self, DescParseError};

#[derive(Debug, thiserror::Error)]
pub enum SyncDbError {
    #[error("failed to read sync db {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported compression in {path} (expected gzip, zstd, xz, or plain tar)")]
    UnknownCompression { path: String },
    #[error("bad desc entry {entry} in {path}: {source}")]
    BadDesc {
        path: String,
        entry: String,
        source: DescParseError,
    },
}

/// One parsed sync database, e.g. `core`, held fully in memory.
///
/// Sync dbs are small (a few MB of metadata), so we eagerly parse the whole
/// tarball into maps rather than streaming: exact-name lookup and
/// provides-lookup both need to be O(1)-ish for the resolver's inner loop.
#[derive(Debug)]
pub struct SyncDb {
    /// Repo name derived from the filename (`core.db` -> `core`).
    pub name: String,
    /// pkgname -> metadata.
    packages: HashMap<String, PackageMeta>,
    /// provided name (e.g. `libgl`) -> pkgnames that provide it.
    provides_index: HashMap<String, Vec<String>>,
}

impl SyncDb {
    /// Open and fully parse one `.db` file. Compression is sniffed from
    /// magic bytes (official mirrors have shipped both gzip and zstd), then
    /// the tar entries named `*/desc` are parsed; everything else is skipped.
    pub fn load(path: &Path) -> Result<Self, SyncDbError> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let io_err = |source| SyncDbError::Io {
            path: path.display().to_string(),
            source,
        };
        let mut raw = Vec::new();
        File::open(path)
            .and_then(|mut f| f.read_to_end(&mut raw))
            .map_err(io_err)?;
        Self::from_tar_bytes(&name, &raw).map_err(|e| match e {
            LoadError::Io(source) => io_err(source),
            LoadError::UnknownCompression => SyncDbError::UnknownCompression {
                path: path.display().to_string(),
            },
            LoadError::BadDesc { entry, source } => SyncDbError::BadDesc {
                path: path.display().to_string(),
                entry,
                source,
            },
        })
    }

    /// Parse a sync db from raw (possibly compressed) tar bytes. Split out
    /// from [`SyncDb::load`] so tests can feed in-memory fixture tarballs.
    pub fn from_tar_bytes(name: &str, raw: &[u8]) -> Result<Self, LoadError> {
        let decompressed: Vec<u8> = match raw {
            [0x1f, 0x8b, ..] => {
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(raw).read_to_end(&mut out)?;
                out
            }
            [0x28, 0xb5, 0x2f, 0xfd, ..] => zstd::stream::decode_all(raw)?,
            // ustar magic at offset 257 marks a plain tar.
            _ if raw.len() > 262 && &raw[257..262] == b"ustar" => raw.to_vec(),
            _ => return Err(LoadError::UnknownCompression),
        };

        let mut packages = HashMap::new();
        let mut provides_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut archive = tar::Archive::new(decompressed.as_slice());
        for entry in archive.entries()? {
            let mut entry = entry?;
            let entry_path = entry.path()?.display().to_string();
            if !entry_path.ends_with("/desc") {
                continue;
            }
            let mut contents = String::new();
            entry.read_to_string(&mut contents)?;
            let pkg = desc::parse(name, &contents).map_err(|source| LoadError::BadDesc {
                entry: entry_path,
                source,
            })?;
            for provide in &pkg.provides {
                provides_index
                    .entry(provide.name.clone())
                    .or_default()
                    .push(pkg.name.clone());
            }
            packages.insert(pkg.name.clone(), pkg);
        }
        Ok(SyncDb {
            name: name.to_owned(),
            packages,
            provides_index,
        })
    }

    /// Rebuilds a `SyncDb` from an already-parsed package list — the
    /// provides index is cheap to recompute in memory, so a cache only
    /// needs to store the flat package list, not the index itself.
    pub fn from_packages(name: String, packages: Vec<PackageMeta>) -> Self {
        let mut provides_index: HashMap<String, Vec<String>> = HashMap::new();
        for pkg in &packages {
            for provide in &pkg.provides {
                provides_index.entry(provide.name.clone()).or_default().push(pkg.name.clone());
            }
        }
        let packages = packages.into_iter().map(|p| (p.name.clone(), p)).collect();
        Self { name, packages, provides_index }
    }

    pub fn packages(&self) -> impl Iterator<Item = &PackageMeta> {
        self.packages.values()
    }

    pub fn get(&self, name: &str) -> Option<&PackageMeta> {
        self.packages.get(name)
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    pub fn providers_of(&self, dep_name: &str) -> impl Iterator<Item = &PackageMeta> {
        self.provides_index
            .get(dep_name)
            .into_iter()
            .flatten()
            .filter_map(|pkg| self.packages.get(pkg))
    }
}

/// Internal error for [`SyncDb::from_tar_bytes`]; [`SyncDb::load`] enriches
/// it with the file path.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unknown compression format")]
    UnknownCompression,
    #[error("bad desc entry {entry}: {source}")]
    BadDesc {
        entry: String,
        source: DescParseError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory sync-db tarball from (pkgname, desc-contents).
    fn fixture_tar(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (pkg, desc) in entries {
            let path = format!("{pkg}/desc");
            let mut header = tar::Header::new_ustar();
            header.set_size(desc.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, desc.as_bytes())
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn desc(name: &str, version: &str, extra: &str) -> String {
        format!("%NAME%\n{name}\n\n%VERSION%\n{version}\n\n{extra}")
    }

    #[test]
    fn parses_plain_tar() {
        let tarball = fixture_tar(&[
            ("foo-1.0-1", &desc("foo", "1.0-1", "%DEPENDS%\nbar\n")),
            ("bar-2.0-1", &desc("bar", "2.0-1", "%PROVIDES%\nlibbar=2.0\n")),
        ]);
        let db = SyncDb::from_tar_bytes("core", &tarball).unwrap();
        assert_eq!(db.len(), 2);
        assert_eq!(db.get("foo").unwrap().depends[0].name, "bar");
        let providers: Vec<_> = db.providers_of("libbar").map(|p| p.name.as_str()).collect();
        assert_eq!(providers, ["bar"]);
    }

    #[test]
    fn parses_gzip_compressed() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let tarball = fixture_tar(&[("foo-1.0-1", &desc("foo", "1.0-1", ""))]);
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tarball).unwrap();
        let db = SyncDb::from_tar_bytes("core", &enc.finish().unwrap()).unwrap();
        assert!(db.get("foo").is_some());
    }

    #[test]
    fn parses_zstd_compressed() {
        let tarball = fixture_tar(&[("foo-1.0-1", &desc("foo", "1.0-1", ""))]);
        let compressed = zstd::stream::encode_all(tarball.as_slice(), 3).unwrap();
        let db = SyncDb::from_tar_bytes("core", &compressed).unwrap();
        assert!(db.get("foo").is_some());
    }

    #[test]
    fn rejects_garbage() {
        let err = SyncDb::from_tar_bytes("core", b"definitely not a database").unwrap_err();
        assert!(matches!(err, LoadError::UnknownCompression));
    }

    #[test]
    fn non_desc_entries_are_skipped() {
        let tarball = fixture_tar(&[
            ("foo-1.0-1", &desc("foo", "1.0-1", "")),
            // `files` entries exist in .files databases; must be ignored.
        ]);
        let db = SyncDb::from_tar_bytes("core", &tarball).unwrap();
        assert_eq!(db.len(), 1);
    }
}
