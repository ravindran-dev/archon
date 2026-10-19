use serde::{Deserialize, Serialize};

use crate::dependency::Dependency;
use crate::version::{Version, VersionCompareError};

/// Where a package comes from — this decides how archon installs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageSource {
    /// An official repo package (core/extra/multilib/...). Installed by
    /// delegating to `pacman -S --needed`.
    Official { repo: String },
    /// An AUR package. Built from its PKGBUILD (sandboxed in Phase 2+),
    /// then installed with `pacman -U`.
    Aur,
    /// A Debian/Ubuntu package via apt.
    Apt,
}

/// Unified package metadata, whether it came from a sync database or the
/// AUR RPC. This is the only shape the resolver ever sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub name: String,
    pub version: Version,
    pub description: Option<String>,
    pub source: PackageSource,
    /// Shared AUR recipe for split outputs. None for prebuilt/legacy metadata.
    #[serde(default)]
    pub package_base: Option<String>,

    /// Runtime dependencies (`depends=()` / `%DEPENDS%`).
    pub depends: Vec<Dependency>,
    /// Build-time dependencies, including checkdepends because makepkg runs
    /// checks by default. Only meaningful for AUR packages; sync databases
    /// do not ship build prerequisites for prebuilt packages.
    pub make_depends: Vec<Dependency>,
    /// Packages this one conflicts with (`conflicts=()` / `%CONFLICTS%`).
    pub conflicts: Vec<Dependency>,
    /// Virtual packages this one provides, e.g. `libgl` or `jdk=21`.
    pub provides: Vec<Dependency>,
}

impl PackageMeta {
    pub fn build_base(&self) -> &str {
        self.package_base.as_deref().unwrap_or(&self.name)
    }

    pub fn is_aur(&self) -> bool {
        matches!(self.source, PackageSource::Aur)
    }

    /// Whether this package satisfies `dep`, either directly (name matches
    /// and version passes the constraint) or through its `provides` array.
    pub fn satisfies(&self, dep: &Dependency) -> Result<bool, VersionCompareError> {
        if self.name == dep.name && dep.is_satisfied_by(&self.version)? {
            return Ok(true);
        }
        for provide in &self.provides {
            if dep.satisfied_by_provide(provide)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn pkg(name: &str, version: &str, provides: &[&str]) -> PackageMeta {
        PackageMeta {
            name: name.into(),
            version: Version::parse(version).unwrap(),
            description: None,
            source: PackageSource::Official {
                repo: "core".into(),
            },
            package_base: None,
            depends: vec![],
            make_depends: vec![],
            conflicts: vec![],
            provides: provides
                .iter()
                .map(|p| Dependency::parse(p).unwrap())
                .collect(),
        }
    }

    #[test]
    fn satisfies_by_exact_name_and_version() {
        let p = pkg("python", "3.12.1-1", &[]);
        assert!(
            p.satisfies(&Dependency::parse("python>=3.11").unwrap())
                .unwrap()
        );
        assert!(
            !p.satisfies(&Dependency::parse("python>=3.13").unwrap())
                .unwrap()
        );
    }

    #[test]
    fn satisfies_via_provides() {
        let p = pkg("bash", "5.2-1", &["sh"]);
        assert!(p.satisfies(&Dependency::parse("sh").unwrap()).unwrap());
        let jdk = pkg(
            "jdk-openjdk",
            "21.0.2-1",
            &["jdk=21", "java-environment=21"],
        );
        assert!(
            jdk.satisfies(&Dependency::parse("jdk>=17").unwrap())
                .unwrap()
        );
        assert!(
            !jdk.satisfies(&Dependency::parse("jdk>=22").unwrap())
                .unwrap()
        );
    }
}
