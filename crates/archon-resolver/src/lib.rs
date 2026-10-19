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

pub mod graph;
pub mod plan;
pub mod suggest;
mod search;

pub use graph::ResolveError;
pub use plan::{InstallPlan, PlannedPackage};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use archon_types::{Dependency, PackageMeta, PackageProvider};

/// Label used as the "requirer" of packages the user asked for directly.
pub const TARGET: &str = "target";

pub struct Resolver<'a> {
    /// Precedence-ordered: a name found in providers[0] is never looked up
    /// in providers[1]. This mirrors pacman repo ordering and guarantees an
    /// AUR package can't shadow an official one.
    providers: Vec<&'a dyn PackageProvider>,
    /// Names already installed on the system — a pure hint set, not I/O,
    /// so the resolver's mock-based tests stay unaffected unless a test
    /// opts in. See [`Resolver::prefer_installed`] for why this exists.
    prefer_installed: Option<&'a HashSet<String>>,
    backend: archon_types::VersionBackend,
    search_limit: usize,
}

impl<'a> Resolver<'a> {
    pub fn new(providers: Vec<&'a dyn PackageProvider>) -> Self {
        Self { providers, prefer_installed: None, backend: archon_types::VersionBackend::Alpm, search_limit: 100_000 }
    }

    /// When a virtual dependency (`provides=()`) can be satisfied by more
    /// than one candidate, prefer one already in `installed_names` over
    /// picking alphabetically. Without this, resolving e.g. `jack` when
    /// both `jack2` and `pipewire-jack` provide it picks whichever sorts
    /// first regardless of which one (if either) is already on the
    /// system — which, since the two conflict with each other, can mean
    /// resolving to a *new* package that requires *removing* an
    /// already-installed one that was already satisfying the dependency
    /// just fine. Found live: `archon install okular` proposed removing
    /// an installed `pipewire-jack` to install `jack2` for exactly this
    /// reason.
    pub fn prefer_installed(mut self, installed_names: &'a HashSet<String>) -> Self {
        self.prefer_installed = Some(installed_names);
        self
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
        search::resolve(self, targets)
    }

    /// Select version semantics for target constraints. Arch is the default;
    /// callers resolving Debian metadata must explicitly choose Dpkg.
    pub fn with_version_backend(mut self, backend: archon_types::VersionBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Bound search work; exhaustion is an explicit error, never NotFound.
    pub fn with_search_limit(mut self, steps: usize) -> Self {
        self.search_limit = steps;
        self
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
    if let Some(satisfier) = find_satisfier(resolved, dep)? {
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
) -> Result<Option<&'r PackageMeta>, ResolveError> {
    if let Some(meta) = resolved.get(&dep.name)
        && dep.is_satisfied_by(&meta.version)?
    {
        return Ok(Some(meta));
    }
    let mut packages: Vec<_> = resolved.values().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    for meta in packages {
        if meta.satisfies(dep)? { return Ok(Some(meta)); }
    }
    Ok(None)
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

/// A conflict in either direction between two selected candidates.
fn package_conflict(candidate: &PackageMeta, existing: &PackageMeta) -> Result<Option<ResolveError>, ResolveError> {
    for (declaring, other) in [(candidate, existing), (existing, candidate)] {
        for conflict in &declaring.conflicts {
            if conflict.name != declaring.name && other.satisfies(conflict)? {
                return Ok(Some(ResolveError::PackageConflict {
                    a: declaring.name.clone(), b: other.name.clone(), declared_by: declaring.name.clone(),
                }));
            }
        }
    }
    Ok(None)
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
                if other.name != pkg.name && other.satisfies(conflict)? {
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
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in &aur_nodes {
        groups.entry(resolved[name].build_base().to_string()).or_default().insert(name.clone());
    }
    let group_nodes: BTreeSet<String> = groups.keys().cloned().collect();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Use the concrete choices recorded during resolution. Looking up the
    // original dependency again loses alternative choices and may pick a
    // different virtual provider from the unordered resolved map.
    for (dependency, users) in requirers {
        if !aur_nodes.contains(dependency) {
            continue;
        }
        for (dependent, spec) in users {
            // TARGET is also a legal package name. A target request must
            // not become an edge to a package literally named "target".
            let Some(meta) = resolved.get(dependent).filter(|m| m.is_aur()) else {
                continue;
            };
            let declared = meta.depends.iter().chain(meta.make_depends.iter()).any(|dep| {
                dep.to_string() == *spec || dep.alternatives.iter().any(|alt| alt.to_string() == *spec)
            });
            let dependency_base = resolved[dependency].build_base();
            let dependent_base = meta.build_base();
            if declared && dependency_base != dependent_base {
                edges.entry(dependency_base.to_string()).or_default().insert(dependent_base.to_string());
            }
        }
    }
    let aur_order: Vec<String> = graph::toposort(&group_nodes, &edges)?
        .into_iter().flat_map(|base| groups[&base].iter().cloned().collect::<Vec<_>>()).collect();

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
            package_base: if aur { Some(name.into()) } else { None },
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
    fn aur_alternative_is_built_before_its_dependent() {
        let mut scanner = pkg(true, "a-scanner", "1", &[], &[], &[], &[]);
        let mut dependency = Dependency::parse("missing-library").unwrap();
        dependency.alternatives.push(Dependency::parse("z-library").unwrap());
        scanner.depends.push(dependency);
        let plan = resolve(
            vec![],
            vec![scanner, pkg(true, "z-library", "1", &[], &[], &[], &[])],
            &["a-scanner"],
        ).unwrap();
        assert_eq!(names(&plan.aur), ["z-library", "a-scanner"]);
    }

    #[test]
    fn virtual_provider_avoids_conflict_with_selected_package() {
        let plan = resolve(
            vec![
                pkg(false, "scanner", "1", &["tls-api"], &[], &[], &[]),
                pkg(false, "openssl", "1", &[], &[], &[], &[]),
                pkg(false, "a-tls", "1", &["rejected-only-dependency"], &[], &["openssl"], &["tls-api"]),
                pkg(false, "z-tls", "1", &[], &[], &[], &["tls-api"]),
            ],
            vec![],
            &["scanner", "openssl"],
        ).unwrap();
        assert_eq!(names(&plan.official), ["openssl", "scanner", "z-tls"]);
    }

    #[test]
    fn virtual_provider_respects_reverse_conflict_even_when_installed() {
        let official = MockProvider::official(vec![
            pkg(false, "scanner", "1", &["tls-api"], &[], &[], &[]),
            pkg(false, "openssl", "1", &[], &[], &["a-tls"], &[]),
            pkg(false, "a-tls", "1", &[], &[], &[], &["tls-api"]),
            pkg(false, "z-tls", "1", &[], &[], &[], &["tls-api"]),
        ]);
        let installed = HashSet::from(["a-tls".to_string()]);
        let plan = Resolver::new(vec![&official]).prefer_installed(&installed)
            .resolve(&["scanner".into(), "openssl".into()]).unwrap();
        assert_eq!(names(&plan.official), ["openssl", "scanner", "z-tls"]);
    }

    #[test]
    fn all_virtual_providers_conflict_reports_conflict_before_dependencies() {
        let result = resolve(
            vec![
                pkg(false, "scanner", "1", &["tls-api"], &[], &[], &[]),
                pkg(false, "openssl", "1", &[], &[], &[], &[]),
                pkg(false, "a-tls", "1", &["unavailable"], &[], &["openssl"], &["tls-api"]),
            ],
            vec![],
            &["scanner", "openssl"],
        );
        assert!(matches!(result, Err(ResolveError::PackageConflict { .. })), "{result:?}");
    }

    #[test]
    fn aur_package_named_target_does_not_create_spurious_edges() {
        let plan = resolve(
            vec![],
            vec![
                pkg(true, "target", "1", &[], &[], &[], &[]),
                pkg(true, "scanner", "1", &["target"], &[], &[], &[]),
            ],
            &["target", "scanner"],
        ).unwrap();
        assert_eq!(names(&plan.aur), ["target", "scanner"]);
    }

    #[test]
    fn backtracks_alternative_after_late_conflict_without_leaked_dependencies() {
        let mut app = pkg(false, "app", "1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("bad").unwrap();
        dep.alternatives.push(Dependency::parse("good").unwrap());
        app.depends.push(dep);
        let plan = resolve(vec![
            app,
            pkg(false, "later", "1", &["blocker"], &[], &[], &[]),
            pkg(false, "bad", "1", &["private-child"], &[], &[], &[]),
            pkg(false, "private-child", "1", &[], &[], &[], &[]),
            pkg(false, "blocker", "1", &[], &[], &["bad"], &[]),
            pkg(false, "good", "1", &[], &[], &[], &[]),
        ], vec![], &["app", "later"]).unwrap();
        assert_eq!(names(&plan.official), ["app", "blocker", "good", "later"]);
    }

    #[test]
    fn backtracks_virtual_provider_after_late_conflict() {
        let plan = resolve(vec![
            pkg(false, "app", "1", &["tls-api"], &[], &[], &[]),
            pkg(false, "later", "1", &["blocker"], &[], &[], &[]),
            pkg(false, "a-provider", "1", &["private-child"], &[], &[], &["tls-api"]),
            pkg(false, "private-child", "1", &[], &[], &[], &[]),
            pkg(false, "blocker", "1", &[], &[], &["a-provider"], &[]),
            pkg(false, "z-provider", "1", &[], &[], &[], &["tls-api"]),
        ], vec![], &["app", "later"]).unwrap();
        assert_eq!(names(&plan.official), ["app", "blocker", "later", "z-provider"]);
    }

    #[test]
    fn mixed_backend_resolution_returns_typed_error() {
        let mut candidate = pkg(false, "tool", "1", &[], &[], &[], &[]);
        candidate.version = Version::parse_dpkg("1").unwrap();
        let result = resolve(vec![candidate], vec![], &["tool>=1"]);
        assert!(matches!(result, Err(ResolveError::Version(_))));
    }

    #[test]
    fn backtracking_restores_already_selected_dependencies() {
        let mut app = pkg(false, "app", "1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("bad").unwrap();
        dep.alternatives.push(Dependency::parse("good").unwrap());
        app.depends.push(dep);
        let plan = resolve(vec![
            app,
            pkg(false, "later", "1", &["middle"], &[], &[], &[]),
            pkg(false, "middle", "1", &["blocker"], &[], &[], &[]),
            pkg(false, "bad", "1", &["private-child"], &[], &[], &[]),
            pkg(false, "private-child", "1", &[], &[], &[], &[]),
            pkg(false, "blocker", "1", &[], &[], &["bad"], &[]),
            pkg(false, "good", "1", &[], &[], &[], &[]),
        ], vec![], &["app", "later"]).unwrap();
        assert_eq!(names(&plan.official), ["app", "blocker", "good", "later", "middle"]);
    }

    #[test]
    fn backtracking_recovers_from_missing_transitive_dependency() {
        let mut app = pkg(false, "app", "1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("bad").unwrap();
        dep.alternatives.push(Dependency::parse("good").unwrap());
        app.depends.push(dep);
        let plan = resolve(vec![app,
            pkg(false, "bad", "1", &["unavailable"], &[], &[], &[]),
            pkg(false, "good", "1", &[], &[], &[], &[]),
        ], vec![], &["app"]).unwrap();
        assert_eq!(names(&plan.official), ["app", "good"]);
    }

    #[test]
    fn backtracking_recovers_from_aur_cycle() {
        let mut app = pkg(true, "app", "1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("cyclic").unwrap();
        dep.alternatives.push(Dependency::parse("good").unwrap());
        app.depends.push(dep);
        let plan = resolve(vec![], vec![app,
            pkg(true, "cyclic", "1", &["app"], &[], &[], &[]),
            pkg(true, "good", "1", &[], &[], &[], &[]),
        ], &["app"]).unwrap();
        assert_eq!(names(&plan.aur), ["good", "app"]);
    }

    #[test]
    fn search_exhaustion_is_explicit() {
        let provider = MockProvider::official(vec![pkg(false, "app", "1", &["lib"], &[], &[], &[])]);
        let result = Resolver::new(vec![&provider]).with_search_limit(1).resolve(&["app".into()]);
        assert!(matches!(result, Err(ResolveError::SearchLimit { .. })));
    }

    #[test]
    fn debian_target_constraints_use_dpkg_backend() {
        let mut candidate = pkg(false, "tool", "1", &[], &[], &[], &[]);
        candidate.source = PackageSource::Apt;
        candidate.version = Version::parse_dpkg("1.0~rc1").unwrap();
        let provider = MockProvider::official(vec![candidate]);
        let plan = Resolver::new(vec![&provider]).with_version_backend(archon_types::VersionBackend::Dpkg)
            .resolve(&["tool<1.0".into()]).unwrap();
        assert_eq!(names(&plan.official), ["tool"]);
    }

    #[test]
    fn split_siblings_with_runtime_cycle_build_as_one_base() {
        let mut a = pkg(true, "suite-a", "1", &["suite-b"], &[], &[], &[]);
        let mut b = pkg(true, "suite-b", "1", &["suite-a"], &[], &[], &[]);
        a.package_base = Some("suite".into());
        b.package_base = Some("suite".into());
        let plan = resolve(vec![], vec![a, b], &["suite-a"]).unwrap();
        assert_eq!(names(&plan.aur), ["suite-a", "suite-b"]);
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
    fn provides_resolved_package_is_also_found_by_concrete_name_in_same_batch() {
        let plan = resolve(
            vec![
                pkg(false, "app", "1.0-1", &["libgl", "mesa"], &[], &[], &[]),
                pkg(false, "mesa", "24.0-1", &[], &[], &[], &["libgl"]),
            ],
            vec![],
            &["app"],
        )
        .unwrap();
        assert_eq!(names(&plan.official), ["app", "mesa"]);
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

    /// Regression test for a real bug found via live testing: `archon
    /// install firefox` failed with "'jack' conflicts with 'jack2'" because
    /// the resolver tried exact-name matches across *all* providers before
    /// ever trying `provides=()` fallback within a single provider. There is
    /// no official package literally named `jack` (only `jack2` and
    /// `pipewire-jack`, which both declare `provides=(jack)` and correctly
    /// conflict with each other) — but the AUR still hosts a legacy package
    /// literally named `jack`. The resolver picked *that* via exact match
    /// before ever trying official's provides fallback, then correctly (but
    /// pointlessly) flagged it as conflicting with jack2. Real pacman never
    /// even considers the AUR here, so it never hits this at all — an
    /// official virtual dependency must be satisfied within official
    /// providers before ever consulting the AUR, exact match or not.
    #[test]
    fn official_provides_beats_aur_exact_name_match() {
        let plan = resolve(
            vec![
                pkg(false, "app", "1.0-1", &["jack"], &[], &[], &[]),
                pkg(false, "jack2", "1.9-1", &[], &[], &["jack"], &["jack"]),
            ],
            vec![pkg(true, "jack", "1.0-1", &[], &[], &["jack2"], &[])],
            &["app"],
        )
        .unwrap();
        assert!(
            plan.official.iter().any(|p| p.meta.name == "jack2"),
            "official jack2 should satisfy the virtual `jack` dependency"
        );
        assert!(
            plan.aur.is_empty(),
            "the AUR's literal `jack` package must not be pulled in when \
             official jack2 already satisfies the dependency via provides"
        );
    }

    /// Regression test for a real bug found via live testing: `archon
    /// install okular` proposed removing an already-installed
    /// `pipewire-jack` to install `jack2` instead — both provide the
    /// virtual `jack` dependency and conflict with each other, and the
    /// resolver picked between them purely alphabetically, with no idea
    /// one of them was already satisfying the need. `prefer_installed`
    /// must make it pick the one already on the system instead.
    #[test]
    fn prefer_installed_avoids_unnecessary_conflicting_swap() {
        let official = MockProvider::official(vec![
            pkg(false, "app", "1.0-1", &["jack"], &[], &[], &[]),
            pkg(false, "jack2", "1.9-1", &[], &[], &["pipewire-jack"], &["jack"]),
            pkg(false, "pipewire-jack", "1.6-1", &[], &[], &["jack2"], &["jack"]),
        ]);
        let aur = MockProvider::aur(vec![]);
        let installed: HashSet<String> = ["pipewire-jack".to_string()].into_iter().collect();

        let plan = Resolver::new(vec![&official, &aur])
            .prefer_installed(&installed)
            .resolve(&["app".to_string()])
            .unwrap();

        assert!(
            names(&plan.official).contains(&"pipewire-jack"),
            "should keep the already-installed provider: {:?}",
            names(&plan.official)
        );
        assert!(
            !names(&plan.official).contains(&"jack2"),
            "should not pull in a conflicting alternative when the \
             already-installed one already satisfies the dependency: {:?}",
            names(&plan.official)
        );
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
    fn alternatives_fallback_if_primary_fails() {
        let mut p = pkg(false, "app", "1.0-1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("missing-pkg").unwrap();
        dep.alternatives.push(Dependency::parse("available-pkg").unwrap());
        p.depends.push(dep);

        let plan = resolve(
            vec![
                p,
                pkg(false, "available-pkg", "1.0-1", &[], &[], &[], &[]),
            ],
            vec![],
            &["app"],
        ).unwrap();
        assert_eq!(names(&plan.official), ["app", "available-pkg"]);
    }

    #[test]
    fn alternatives_fallback_under_conflict() {
        // App depends on "postfix | exim4".
        let mut app = pkg(false, "app", "1.0-1", &[], &[], &[], &[]);
        let mut dep = Dependency::parse("postfix").unwrap();
        dep.alternatives.push(Dependency::parse("exim4").unwrap());
        app.depends.push(dep);

        // postfix and exim4 both exist.
        // But the user ALSO explicitly installed "otherapp", which depends on "exim4"
        // and conflicts with "postfix".
        let otherapp = pkg(false, "otherapp", "1.0-1", &["exim4"], &[], &["postfix"], &[]);

        let plan = resolve(
            vec![
                app,
                otherapp,
                pkg(false, "postfix", "1.0-1", &[], &[], &["exim4"], &[]),
                pkg(false, "exim4", "1.0-1", &[], &[], &["postfix"], &[]),
            ],
            vec![],
            &["app", "otherapp"],
        )
        .unwrap();
        
        assert!(names(&plan.official).contains(&"exim4"));
        assert!(!names(&plan.official).contains(&"postfix"));
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
