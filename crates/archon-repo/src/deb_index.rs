use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use archon_types::{
    ConstraintOp, Dependency, PackageMeta, PackageProvider, PackageSource, ProviderError, Version,
};

pub struct AptLists {
    pub packages: Vec<PackageMeta>,
}

impl AptLists {
    pub fn load_dir(apt_lists_dir: &Path) -> Result<Self, std::io::Error> {
        let mut packages = Vec::new();
        if !apt_lists_dir.exists() {
            return Ok(Self { packages });
        }
        for entry in std::fs::read_dir(apt_lists_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            if file_name.ends_with("_Packages") || file_name.ends_with("_Packages.gz") {
                let parsed = Self::parse_file(&path)?;
                packages.extend(parsed);
            }
        }
        Ok(Self { packages })
    }

    pub fn parse_file(path: &Path) -> Result<Vec<PackageMeta>, std::io::Error> {
        let file = File::open(path)?;
        if path.extension().is_some_and(|ext| ext == "gz") {
            let decoder = flate2::read::GzDecoder::new(file);
            Self::parse_reader(BufReader::new(decoder))
        } else {
            Self::parse_reader(BufReader::new(file))
        }
    }

    pub fn parse_reader<R: BufRead>(reader: R) -> Result<Vec<PackageMeta>, std::io::Error> {
        let mut packages = Vec::new();
        let mut current_pkg = None;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                if let Some(pkg) = current_pkg.take()
                    && let Ok(meta) = Self::build_meta(pkg)
                {
                    packages.push(meta);
                }
                continue;
            }

            if line.starts_with(' ') || line.starts_with('\t') {
                // Continuation line (e.g. for Description). Ignore for now, or append to last field.
                continue;
            }

            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                let value = v.trim().to_string();

                let mut pkg: std::collections::HashMap<String, String> =
                    current_pkg.unwrap_or_default();
                pkg.insert(key.to_string(), value);
                current_pkg = Some(pkg);
            }
        }

        if let Some(pkg) = current_pkg
            && let Ok(meta) = Self::build_meta(pkg)
        {
            packages.push(meta);
        }

        Ok(packages)
    }

    fn build_meta(
        mut raw: std::collections::HashMap<String, String>,
    ) -> Result<PackageMeta, &'static str> {
        let name = raw.remove("Package").ok_or("Missing Package")?;
        let version_str = raw.remove("Version").ok_or("Missing Version")?;
        let version = Version::parse_dpkg(&version_str).map_err(|_| "Invalid Version")?;
        let description = raw.remove("Description");

        let mut depends = Vec::new();
        if let Some(deps) = raw.get("Depends") {
            depends = parse_depends(deps);
        }

        let mut provides = Vec::new();
        if let Some(provs) = raw.get("Provides") {
            provides = parse_depends(provs);
        }

        let mut conflicts = Vec::new();
        if let Some(confs) = raw.get("Conflicts") {
            conflicts = parse_depends(confs);
        }

        Ok(PackageMeta {
            package_base: None,
            name,
            version,
            description,
            source: PackageSource::Apt,
            depends,
            make_depends: vec![],
            conflicts,
            provides,
        })
    }

    pub fn all_package_names(&self) -> Vec<(String, String)> {
        self.packages
            .iter()
            .map(|p| (p.name.clone(), "apt".to_string()))
            .collect()
    }
}

impl PackageProvider for AptLists {
    fn label(&self) -> &str {
        "apt"
    }

    fn get(&self, names: &[String]) -> Result<Vec<PackageMeta>, ProviderError> {
        let mut out = Vec::new();
        for pkg in &self.packages {
            if names.contains(&pkg.name) {
                out.push(pkg.clone());
            }
        }
        // Deduplicate or pick highest version? For apt, usually highest version wins.
        // Let's just return what we find for now.
        Ok(out)
    }

    fn find_providers(&self, dep_name: &str) -> Result<Vec<PackageMeta>, ProviderError> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for pkg in &self.packages {
            if pkg
                .satisfies(&Dependency {
                    name: dep_name.to_string(),
                    constraint: None,
                    alternatives: Vec::new(),
                })
                .map_err(|error| ProviderError::Backend(Box::new(error)))?
                && seen.insert(pkg.name.clone())
            {
                out.push(pkg.clone());
            }
        }
        Ok(out)
    }
}

/// Parse Debian dependency string `pkg1 (>= 1.0) | pkg2, pkg3`
pub fn parse_depends(raw: &str) -> Vec<Dependency> {
    let mut out = Vec::new();
    for group in raw.split(',') {
        let mut parts = group.split('|').map(|s| s.trim()).filter(|s| !s.is_empty());

        let preferred = match parts.next() {
            Some(p) => p,
            None => continue,
        };

        if let Some(mut dep) = parse_single_depend(preferred) {
            for alt in parts {
                if let Some(alt_dep) = parse_single_depend(alt) {
                    dep.alternatives.push(alt_dep);
                }
            }
            out.push(dep);
        }
    }
    out
}

fn parse_single_depend(raw: &str) -> Option<Dependency> {
    // e.g. "libc6 (>= 2.31)" or "libcrypt1"
    let raw = raw.trim();
    if let Some(idx) = raw.find('(') {
        let name = raw[..idx].trim().to_string();
        let rest = &raw[idx + 1..];
        if let Some(end_idx) = rest.find(')') {
            let constraint_str = rest[..end_idx].trim();
            // constraint_str is like ">= 1.0" or "= 1.0"
            // Let's map it to Archon's VersionConstraint
            let mut op = None;
            let mut ver_str = constraint_str;
            if let Some(v) = constraint_str.strip_prefix(">=") {
                op = Some(ConstraintOp::Ge);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix("<=") {
                op = Some(ConstraintOp::Le);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix("=") {
                op = Some(ConstraintOp::Eq);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix(">>") {
                op = Some(ConstraintOp::Gt);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix("<<") {
                op = Some(ConstraintOp::Lt);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix(">") {
                op = Some(ConstraintOp::Gt);
                ver_str = v.trim();
            } else if let Some(v) = constraint_str.strip_prefix("<") {
                op = Some(ConstraintOp::Lt);
                ver_str = v.trim();
            }

            if let (Some(op), Ok(ver)) = (op, Version::parse_dpkg(ver_str)) {
                return Some(Dependency {
                    name,
                    constraint: Some(archon_types::VersionConstraint { op, version: ver }),
                    alternatives: Vec::new(),
                });
            } else {
                // Ignore invalid constraint, just depend on the name
                return Some(Dependency {
                    name,
                    constraint: None,
                    alternatives: Vec::new(),
                });
            }
        }
    }
    Some(Dependency {
        name: raw.to_string(),
        constraint: None,
        alternatives: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_depends() {
        let raw = "libc6 (>= 2.31-13+deb11u5), libcrypt1 (>= 1:4.4.18-4), libgcc-s1 | some-other";
        let deps = parse_depends(raw);
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "libc6");
        assert_eq!(deps[0].constraint.as_ref().unwrap().op, ConstraintOp::Ge);
        assert_eq!(
            deps[0].constraint.as_ref().unwrap().version.to_string(),
            "2.31-13+deb11u5"
        );

        assert_eq!(deps[1].name, "libcrypt1");
        assert_eq!(deps[2].name, "libgcc-s1");
        assert!(deps[2].constraint.is_none());
    }

    #[test]
    fn test_deb_index_parsing() {
        let stanza = "\
Package: libc6
Version: 2.31-13+deb11u5
Architecture: amd64
Maintainer: GNU Libc Maintainers <debian-glibc@lists.debian.org>
Depends: libcrypt1 (>= 1:4.4.18-4), libgcc-s1
Provides: glibc-2.31-1

Package: libcrypt1
Version: 1:4.4.18-4
Architecture: amd64
";
        let pkgs = AptLists::parse_reader(stanza.as_bytes()).unwrap();
        assert_eq!(pkgs.len(), 2);

        let libc6 = &pkgs[0];
        assert_eq!(libc6.name, "libc6");
        assert_eq!(libc6.version.to_string(), "2.31-13+deb11u5");
        assert_eq!(libc6.depends.len(), 2);
        assert_eq!(libc6.provides.len(), 1);
        assert_eq!(libc6.provides[0].name, "glibc-2.31-1");
    }

    #[test]
    fn test_virtual_package_resolution() {
        let stanza = "\
Package: postfix
Version: 3.5.6-1+b1
Provides: mail-transport-agent

Package: exim4
Version: 4.94.2-7
Provides: mail-transport-agent
";
        let apt = AptLists {
            packages: AptLists::parse_reader(stanza.as_bytes()).unwrap(),
        };
        let providers = apt.find_providers("mail-transport-agent").unwrap();
        assert_eq!(providers.len(), 2);
        let mut names = providers
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["exim4", "postfix"]);
    }

    #[test]
    fn test_tilde_version_ordering() {
        let stanza = "\
Package: test-pkg
Version: 1.0~rc1-1
Architecture: amd64

Package: test-pkg
Version: 1.0-1
Architecture: amd64
";
        let pkgs = AptLists::parse_reader(stanza.as_bytes()).unwrap();
        assert_eq!(pkgs.len(), 2);

        let rc = &pkgs[0].version;
        let final_v = &pkgs[1].version;

        // 1.0~rc1 should be LESS than 1.0
        assert_eq!(rc.vercmp(final_v).unwrap(), std::cmp::Ordering::Less);
    }
}
