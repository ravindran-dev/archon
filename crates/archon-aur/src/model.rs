use serde::Deserialize;

use archon_types::{Dependency, PackageMeta, PackageSource, Version};

use crate::AurError;

/// Wire format of one result object from the AUR RPC v5.
/// Field names follow the RPC's PascalCase JSON.
///
/// Note: `search` results carry only a subset of fields (no dependency
/// arrays), which is why everything list-like defaults to empty — full
/// metadata always comes from a follow-up `info` call.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AurPackage {
    pub name: String,
    pub version: String,
    /// Base package for split packages; `makepkg` builds the base, and the
    /// clone URL is derived from it, not from `name`.
    pub package_base: String,
    pub description: Option<String>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub make_depends: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    /// Unix timestamp; surfaced in install plans so the user can spot
    /// abandoned packages.
    pub out_of_date: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RpcResponse {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub results: Vec<AurPackage>,
    pub error: Option<String>,
}

impl AurPackage {
    /// Convert wire format to archon's unified metadata.
    /// Fails only on malformed version/dependency strings, which the AUR
    /// does occasionally serve — the error must name the offending package.
    pub fn into_meta(self) -> Result<PackageMeta, AurError> {
        let bad = |detail: String| AurError::BadMetadata {
            name: self.name.clone(),
            detail,
        };
        let parse_deps = |list: &[String]| -> Result<Vec<Dependency>, AurError> {
            list.iter()
                .map(|s| Dependency::parse(s).map_err(|e| bad(format!("dependency {s:?}: {e}"))))
                .collect()
        };
        Ok(PackageMeta {
            version: Version::parse(&self.version)
                .map_err(|e| bad(format!("version {:?}: {e}", self.version)))?,
            depends: parse_deps(&self.depends)?,
            make_depends: parse_deps(&self.make_depends)?,
            conflicts: parse_deps(&self.conflicts)?,
            provides: parse_deps(&self.provides)?,
            name: self.name,
            description: self.description,
            source: PackageSource::Aur,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_rpc_json_and_converts() {
        let json = r#"{
            "Name": "paru",
            "Version": "2.0.4-1",
            "PackageBase": "paru",
            "Description": "AUR helper",
            "Depends": ["git", "pacman>=6.1"],
            "MakeDepends": ["cargo"],
            "OutOfDate": null
        }"#;
        let pkg: AurPackage = serde_json::from_str(json).unwrap();
        let meta = pkg.into_meta().unwrap();
        assert_eq!(meta.name, "paru");
        assert!(meta.is_aur());
        assert_eq!(meta.depends[1].to_string(), "pacman>=6.1");
        assert_eq!(meta.make_depends[0].name, "cargo");
        assert!(meta.provides.is_empty());
    }

    #[test]
    fn bad_version_error_names_the_package() {
        let pkg = AurPackage {
            name: "broken".into(),
            version: ":".into(),
            package_base: "broken".into(),
            description: None,
            depends: vec![],
            make_depends: vec![],
            conflicts: vec![],
            provides: vec![],
            out_of_date: None,
        };
        let err = pkg.into_meta().unwrap_err();
        assert!(err.to_string().contains("broken"), "got: {err}");
    }
}
