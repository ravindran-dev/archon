//! The dependency resolver — archon's core.
//!
//! Input: target package names + an ordered list of [`PackageProvider`]s
//! (official repos first, AUR as fallback). Output: an [`InstallPlan`] or a
//! precise [`ResolveError`].
//!
//! The resolver is deliberately pure: it does no I/O of its own, only
//! provider calls. That's what makes it unit-testable with in-memory mock
//! providers on any machine.
//!
//! Resolution walks the dependency graph breadth-first, batching unknown
//! names per level (one AUR round-trip per BFS level instead of per
//! package). For AUR packages, `make_depends` are edges too — you can't
//! build without them.

mod graph;
mod plan;

pub use graph::ResolveError;
pub use plan::{InstallPlan, PlannedPackage};

use std::collections::{BTreeMap, BTreeSet, HashMap};

use archon_types::{Dependency, PackageMeta, PackageProvider};

/// Label used as the "requirer" of packages the user asked for directly.
const TARGET: &str = "target";

pub struct Resolver<'a> {
    /// Precedence-ordered: a name found in providers[0] is never looked up
    /// in providers[1]. This mirrors pacman repo ordering and guarantees an
    /// AUR package can't shadow an official one.
    providers: Vec<&'a dyn PackageProvider>,
}

impl<'a> Resolver<'a> {
    pub fn new(providers: Vec<&'a dyn PackageProvider>) -> Self {
        Self { providers }
    }

    /// Resolve `targets` (plain names or specs like `foo>=2`) to a full
    /// install plan:
    ///
    /// 1. BFS the dependency graph, resolving each [`Dependency`] to a
    ///    concrete package (exact name first, then `provides` fallback).
    /// 2. Check version constraints on the fly and `conflicts` across the
    ///    final set.
    /// 3. Topologically sort the AUR subgraph for build order. Official
    ///    packages aren't ordered — they go to pacman as one transaction.
    pub fn resolve(&self, targets: &[String]) -> Result<InstallPlan, ResolveError> {
        let mut frontier: Vec<(Dependency, String)> = Vec::new();
        for t in targets {
            let dep = Dependency::parse(t).map_err(|_| ResolveError::NotFound {
                name: t.clone(),
                required_by: TARGET.into(),
            })?;
            frontier.push((dep, TARGET.into()));
        }

        // pkgname -> resolved metadata.
        let mut resolved: HashMap<String, PackageMeta> = HashMap::new();
        // pkgname -> [(requirer, dep spec as written)]; drives both
        // `required_by` display and version-conflict error messages.
        let mut requirers: HashMap<String, Vec<(String, String)>> = HashMap::new();

        while !frontier.is_empty() {
            // Phase A: drop deps already satisfied by the resolved set, and
            // surface constraint clashes against already-chosen packages.
            let mut pending: Vec<(Dependency, String)> = Vec::new();
            for (dep, requirer) in frontier.drain(..) {
                if try_satisfy(&resolved, &mut requirers, &dep, &requirer)? {
                    continue;
                }
                pending.push((dep, requirer));
            }
            if pending.is_empty() {
                break;
            }

            // Phase B: batched exact-name lookup, provider order = precedence.
            let mut names: Vec<String> = Vec::new();
            for (dep, _) in &pending {
                if !names.contains(&dep.name) {
                    names.push(dep.name.clone());
                }
            }
            let mut found: HashMap<String, PackageMeta> = HashMap::new();
            let mut missing = names;
            for provider in &self.providers {
                if missing.is_empty() {
                    break;
                }
                for meta in provider.get(&missing)? {
                    missing.retain(|n| *n != meta.name);
                    found.entry(meta.name.clone()).or_insert(meta);
                }
            }

            // Phase C: select a package per pending dep; push its edges.
            for (dep, requirer) in pending {
                // A dep earlier in this very batch may have already
                // resolved a package that satisfies — or conflicts with —
                // this one; recheck with the same logic as Phase A.
                if try_satisfy(&resolved, &mut requirers, &dep, &requirer)? {
                    continue;
                }

                let exact = found.get(&dep.name);
                let selected = match exact {
                    Some(meta) if dep.is_satisfied_by(&meta.version) => Some(meta.clone()),
                    _ => self.find_provider_fallback(&dep)?,
                };
                let Some(meta) = selected else {
                    return Err(match exact {
                        // The name exists but no version/provide satisfies.
                        Some(meta) => ResolveError::NoSatisfyingVersion {
                            name: meta.name.clone(),
                            dep: dep.to_string(),
                            required_by: requirer,
                            available: meta.version.to_string(),
                        },
                        None => ResolveError::NotFound {
                            name: dep.name.clone(),
                            required_by: requirer,
                        },
                    });
                };

                note_requirer(&mut requirers, &meta.name, &requirer, &dep);
                let mut edges: Vec<&Dependency> = meta.depends.iter().collect();
                if meta.is_aur() {
                    // Build-time deps are install-order edges for AUR pkgs.
                    edges.extend(meta.make_depends.iter());
                }
                for edge in edges {
                    frontier.push((edge.clone(), meta.name.clone()));
                }
                resolved.insert(meta.name.clone(), meta);
            }
        }

        check_conflicts(&resolved)?;
        build_plan(resolved, &requirers)
    }

    /// Resolve a dep through `provides` when no exact-name package
    /// satisfies it. First provider (in precedence order) with a satisfying
    /// candidate wins; among candidates, lexicographically smallest name —
    /// deterministic, and the choice is visible in the printed plan.
    fn find_provider_fallback(
        &self,
        dep: &Dependency,
    ) -> Result<Option<PackageMeta>, ResolveError> {
        for provider in &self.providers {
            let mut candidates: Vec<PackageMeta> = provider
                .find_providers(&dep.name)?
                .into_iter()
                .filter(|m| m.satisfies(dep))
                .collect();
            if !candidates.is_empty() {
                candidates.sort_by(|a, b| a.name.cmp(&b.name));
                return Ok(Some(candidates.swap_remove(0)));
            }
        }
        Ok(None)
    }
}

/// Attempt to satisfy `dep` purely from what's already resolved.
///
/// - `Ok(true)`: satisfied by an existing pick (requirer noted).
/// - `Ok(false)`: nothing resolved yet covers this name — caller must look
///   it up fresh.
/// - `Err`: the name is already resolved to a package that does *not*
///   satisfy `dep` — two requirers want incompatible versions.
///
/// Used identically by both BFS phases: Phase A checks against packages
/// resolved in earlier levels, Phase C rechecks against packages resolved
/// earlier in the *same* level (since Phase C mutates `resolved` as it
/// goes), so a single implementation keeps the conflict check honest
/// regardless of which pass first resolves the conflicting name.
fn try_satisfy(
    resolved: &HashMap<String, PackageMeta>,
    requirers: &mut HashMap<String, Vec<(String, String)>>,
    dep: &Dependency,
    requirer: &str,
) -> Result<bool, ResolveError> {
    if let Some(satisfier) = find_satisfier(resolved, dep) {
        let name = satisfier.name.clone();
        note_requirer(requirers, &name, requirer, dep);
        return Ok(true);
    }
    if let Some(existing) = resolved.get(&dep.name) {
        let (first_req, first_con) = requirers
            .get(&existing.name)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| (TARGET.into(), existing.name.clone()));
        return Err(ResolveError::VersionConflict {
            name: dep.name.clone(),
            first_requirer: first_req,
            first_constraint: first_con,
            second_requirer: requirer.to_owned(),
            second_constraint: dep.to_string(),
        });
    }
    Ok(false)
}

/// A resolved package satisfying `dep`, by name or provides.
fn find_satisfier<'r>(
    resolved: &'r HashMap<String, PackageMeta>,
    dep: &Dependency,
) -> Option<&'r PackageMeta> {
    if let Some(meta) = resolved.get(&dep.name) {
        if dep.is_satisfied_by(&meta.version) {
            return Some(meta);
        }
    }
    resolved.values().find(|m| m.satisfies(dep))
}

fn note_requirer(
    requirers: &mut HashMap<String, Vec<(String, String)>>,
    pkg: &str,
    requirer: &str,
    dep: &Dependency,
) {
    let entry = requirers.entry(pkg.to_owned()).or_default();
    if !entry.iter().any(|(r, _)| r == requirer) {
        entry.push((requirer.to_owned(), dep.to_string()));
    }
}

/// Pairwise `conflicts=()` check across the resolved set. A package's
/// conflict with its own name is ignored (idiomatic in PKGBUILDs for
/// package renames/provides).
fn check_conflicts(resolved: &HashMap<String, PackageMeta>) -> Result<(), ResolveError> {
    let mut names: Vec<&String> = resolved.keys().collect();
    names.sort();
    for name in names {
        let pkg = &resolved[name];
        for conflict in &pkg.conflicts {
            if conflict.name == pkg.name {
                continue;
            }
            for other in resolved.values() {
                if other.name != pkg.name && other.satisfies(conflict) {
                    return Err(ResolveError::PackageConflict {
                        a: pkg.name.clone(),
                        b: other.name.clone(),
                        declared_by: pkg.name.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn build_plan(
    resolved: HashMap<String, PackageMeta>,
    requirers: &HashMap<String, Vec<(String, String)>>,
) -> Result<InstallPlan, ResolveError> {
    let required_by = |name: &str| -> Vec<String> {
        requirers
            .get(name)
            .map(|v| {
                v.iter()
                    .map(|(r, _)| r.clone())
                    .filter(|r| r != TARGET)
                    .collect()
            })
            .unwrap_or_default()
    };

    // AUR subgraph: edge dep -> dependent when both resolved to AUR.
    let aur_nodes: BTreeSet<String> = resolved
        .values()
        .filter(|m| m.is_aur())
        .map(|m| m.name.clone())
        .collect();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for meta in resolved.values().filter(|m| m.is_aur()) {
        for dep in meta.depends.iter().chain(meta.make_depends.iter()) {
            if let Some(satisfier) = find_satisfier(&resolved, dep) {
                if satisfier.is_aur() && satisfier.name != meta.name {
                    edges
                        .entry(satisfier.name.clone())
                        .or_default()
                        .insert(meta.name.clone());
                }
            }
        }
    }
    let aur_order = graph::toposort(&aur_nodes, &edges)?;

    let mut official: Vec<PlannedPackage> = resolved
        .values()
        .filter(|m| !m.is_aur())
        .map(|m| PlannedPackage {
            required_by: required_by(&m.name),
            meta: m.clone(),
        })
        .collect();
    official.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));

    let aur = aur_order
        .into_iter()
        .map(|name| PlannedPackage {
            required_by: required_by(&name),
            meta: resolved[&name].clone(),
        })
        .collect();

    Ok(InstallPlan { official, aur })
}

#[cfg(test)]
mod tests {
    use super::*;
    use archon_types::{PackageSource, ProviderError, Version};

    /// Declarative in-memory provider: `(name, version, depends,
    /// makedepends, conflicts, provides)` tuples.
    struct MockProvider {
        label: &'static str,
        aur: bool,
        pkgs: Vec<PackageMeta>,
    }

    fn pkg(
        aur: bool,
        name: &str,
        version: &str,
        depends: &[&str],
        make_depends: &[&str],
        conflicts: &[&str],
        provides: &[&str],
    ) -> PackageMeta {
        let deps = |list: &[&str]| {
            list.iter()
                .map(|d| Dependency::parse(d).unwrap())
                .collect::<Vec<_>>()
        };
        PackageMeta {
            name: name.into(),
            version: Version::parse(version).unwrap(),
            description: None,
            source: if aur {
                PackageSource::Aur
            } else {
                PackageSource::Official { repo: "core".into() }
            },
            depends: deps(depends),
            make_depends: deps(make_depends),
            conflicts: deps(conflicts),
            provides: deps(provides),
        }
    }

    impl MockProvider {
        fn official(pkgs: Vec<PackageMeta>) -> Self {
            Self { label: "official", aur: false, pkgs }
        }
        fn aur(pkgs: Vec<PackageMeta>) -> Self {
            Self { label: "aur", aur: true, pkgs }
        }
    }

    impl PackageProvider for MockProvider {
        fn label(&self) -> &str {
            self.label
        }
        fn get(&self, names: &[String]) -> Result<Vec<PackageMeta>, ProviderError> {
            let _ = self.aur;
            Ok(self
                .pkgs
                .iter()
                .filter(|p| names.contains(&p.name))
                .cloned()
                .collect())
        }
        fn find_providers(&self, dep_name: &str) -> Result<Vec<PackageMeta>, ProviderError> {
            Ok(self
                .pkgs
                .iter()
                .filter(|p| p.provides.iter().any(|pr| pr.name == dep_name))
                .cloned()
                .collect())
        }
    }

    fn resolve(
        official: Vec<PackageMeta>,
        aur: Vec<PackageMeta>,
        targets: &[&str],
    ) -> Result<InstallPlan, ResolveError> {
        let official = MockProvider::official(official);
        let aur = MockProvider::aur(aur);
        let resolver = Resolver::new(vec![&official, &aur]);
        resolver.resolve(&targets.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    fn names(list: &[PlannedPackage]) -> Vec<&str> {
        list.iter().map(|p| p.meta.name.as_str()).collect()
    }

    #[test]
    fn linear_chain_resolves_fully() {
        let plan = resolve(
            vec![
                pkg(false, "a", "1.0-1", &["b"], &[], &[], &[]),
                pkg(false, "b", "1.0-1", &["c"], &[], &[], &[]),
                pkg(false, "c", "1.0-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["a"],
        )
        .unwrap();
        assert_eq!(names(&plan.official), ["a", "b", "c"]);
        assert!(plan.aur.is_empty());
    }

    #[test]
    fn diamond_appears_once() {
        let plan = resolve(
            vec![
                pkg(false, "top", "1.0-1", &["left", "right"], &[], &[], &[]),
                pkg(false, "left", "1.0-1", &["base"], &[], &[], &[]),
                pkg(false, "right", "1.0-1", &["base"], &[], &[], &[]),
                pkg(false, "base", "1.0-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["top"],
        )
        .unwrap();
        assert_eq!(plan.package_count(), 4);
        let base = plan.official.iter().find(|p| p.meta.name == "base").unwrap();
        let mut req = base.required_by.clone();
        req.sort();
        assert_eq!(req, ["left", "right"]);
    }

    #[test]
    fn virtual_package_resolved_via_provides() {
        let plan = resolve(
            vec![
                pkg(false, "script", "1.0-1", &["sh"], &[], &[], &[]),
                pkg(false, "bash", "5.2-1", &[], &[], &[], &["sh"]),
            ],
            vec![],
            &["script"],
        )
        .unwrap();
        assert_eq!(names(&plan.official), ["bash", "script"]);
    }

    #[test]
    fn versioned_provide_checked_against_constraint() {
        // Only jdk21 satisfies jdk>=17; jdk8's provide must be rejected.
        let plan = resolve(
            vec![
                pkg(false, "app", "1.0-1", &["jdk>=17"], &[], &[], &[]),
                pkg(false, "jdk8", "8.0-1", &[], &[], &[], &["jdk=8"]),
                pkg(false, "jdk21", "21.0-1", &[], &[], &[], &["jdk=21"]),
            ],
            vec![],
            &["app"],
        )
        .unwrap();
        assert!(names(&plan.official).contains(&"jdk21"));
        assert!(!names(&plan.official).contains(&"jdk8"));
    }

    #[test]
    fn official_shadows_aur_same_name() {
        let plan = resolve(
            vec![pkg(false, "tool", "2.0-1", &[], &[], &[], &[])],
            vec![pkg(true, "tool", "9.9-1", &[], &[], &[], &[])],
            &["tool"],
        )
        .unwrap();
        assert_eq!(plan.official.len(), 1);
        assert!(plan.aur.is_empty(), "AUR package must not shadow official");
    }

    #[test]
    fn aur_package_pulls_official_deps_and_makedeps() {
        let plan = resolve(
            vec![
                pkg(false, "glibc", "2.39-1", &[], &[], &[], &[]),
                pkg(false, "cargo", "1.78-1", &[], &[], &[], &[]),
            ],
            vec![pkg(true, "paru", "2.0-1", &["glibc"], &["cargo"], &[], &[])],
            &["paru"],
        )
        .unwrap();
        assert_eq!(names(&plan.official), ["cargo", "glibc"]);
        assert_eq!(names(&plan.aur), ["paru"]);
    }

    #[test]
    fn aur_build_order_is_dependency_first() {
        let plan = resolve(
            vec![],
            vec![
                pkg(true, "app", "1.0-1", &["lib"], &[], &[], &[]),
                pkg(true, "lib", "1.0-1", &["base"], &[], &[], &[]),
                pkg(true, "base", "1.0-1", &[], &[], &[], &[]),
            ],
            &["app"],
        )
        .unwrap();
        assert_eq!(names(&plan.aur), ["base", "lib", "app"]);
    }

    #[test]
    fn aur_makedepends_ordered_before_dependent() {
        let plan = resolve(
            vec![],
            vec![
                pkg(true, "app", "1.0-1", &[], &["buildtool"], &[], &[]),
                pkg(true, "buildtool", "1.0-1", &[], &[], &[], &[]),
            ],
            &["app"],
        )
        .unwrap();
        assert_eq!(names(&plan.aur), ["buildtool", "app"]);
    }

    #[test]
    fn version_conflict_names_both_requirers() {
        let err = resolve(
            vec![
                pkg(false, "x", "1.0-1", &["lib>=2"], &[], &[], &[]),
                pkg(false, "y", "1.0-1", &["lib<2"], &[], &[], &[]),
                pkg(false, "lib", "2.5-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["x", "y"],
        )
        .unwrap_err();
        match err {
            ResolveError::VersionConflict { name, .. } => assert_eq!(name, "lib"),
            other => panic!("expected VersionConflict, got: {other}"),
        }
    }

    #[test]
    fn unsatisfiable_version_reports_whats_available() {
        let err = resolve(
            vec![
                pkg(false, "app", "1.0-1", &["lib>=3"], &[], &[], &[]),
                pkg(false, "lib", "2.0-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["app"],
        )
        .unwrap_err();
        match err {
            ResolveError::NoSatisfyingVersion { name, available, required_by, .. } => {
                assert_eq!(name, "lib");
                assert_eq!(available, "2.0-1");
                assert_eq!(required_by, "app");
            }
            other => panic!("expected NoSatisfyingVersion, got: {other}"),
        }
    }

    #[test]
    fn not_found_names_the_dependent() {
        let err = resolve(
            vec![pkg(false, "app", "1.0-1", &["ghost"], &[], &[], &[])],
            vec![],
            &["app"],
        )
        .unwrap_err();
        match err {
            ResolveError::NotFound { name, required_by } => {
                assert_eq!(name, "ghost");
                assert_eq!(required_by, "app");
            }
            other => panic!("expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn declared_conflicts_are_fatal() {
        let err = resolve(
            vec![
                pkg(false, "a", "1.0-1", &[], &[], &["b"], &[]),
                pkg(false, "b", "1.0-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["a", "b"],
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::PackageConflict { .. }), "got: {err}");
    }

    #[test]
    fn self_conflict_is_ignored() {
        // Common PKGBUILD idiom: provides=(foo) conflicts=(foo).
        let plan = resolve(
            vec![],
            vec![pkg(true, "foo-git", "1.0-1", &[], &[], &["foo-git"], &["foo-git"])],
            &["foo-git"],
        );
        assert!(plan.is_ok(), "self-conflict must not be fatal: {:?}", plan.err());
    }

    #[test]
    fn aur_cycle_is_reported_with_path() {
        let err = resolve(
            vec![],
            vec![
                pkg(true, "a", "1.0-1", &["b"], &[], &[], &[]),
                pkg(true, "b", "1.0-1", &["a"], &[], &[], &[]),
            ],
            &["a"],
        )
        .unwrap_err();
        match err {
            ResolveError::DependencyCycle { cycle } => {
                assert!(cycle.len() >= 3, "cycle path should close on itself: {cycle:?}");
                assert_eq!(cycle.first(), cycle.last());
            }
            other => panic!("expected DependencyCycle, got: {other}"),
        }
    }

    #[test]
    fn official_cycles_are_tolerated() {
        // Real official repos contain cycles; pacman installs them in one
        // transaction, so archon must not error.
        let plan = resolve(
            vec![
                pkg(false, "glibc", "2.39-1", &["gcc-libs"], &[], &[], &[]),
                pkg(false, "gcc-libs", "13.2-1", &["glibc"], &[], &[], &[]),
            ],
            vec![],
            &["glibc"],
        );
        assert!(plan.is_ok(), "official cycle must be fine: {:?}", plan.err());
    }

    #[test]
    fn targets_have_empty_required_by() {
        let plan = resolve(
            vec![pkg(false, "app", "1.0-1", &[], &[], &[], &[])],
            vec![],
            &["app"],
        )
        .unwrap();
        assert!(plan.official[0].required_by.is_empty());
    }

    #[test]
    fn versioned_target_spec_is_honored() {
        let err = resolve(
            vec![pkg(false, "app", "1.0-1", &[], &[], &[], &[])],
            vec![],
            &["app>=2"],
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::NoSatisfyingVersion { .. }), "got: {err}");
    }
}
