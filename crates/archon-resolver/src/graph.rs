use std::collections::{BTreeMap, BTreeSet};

use archon_types::ProviderError;

/// Resolution failures. Every variant carries enough context to print a
/// message that tells the user *what to do*, not just what broke —
/// "package X (required by Y) not found in official repos or AUR" beats
/// "resolution failed".
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("package '{name}' not found in official repos or the AUR (required by {required_by})")]
    NotFound { name: String, required_by: String },

    #[error(
        "no package satisfies '{dep}' (required by {required_by}); \
         found only {name} {available}"
    )]
    NoSatisfyingVersion {
        name: String,
        dep: String,
        required_by: String,
        available: String,
    },

    #[error(
        "version conflict on '{name}': {first_requirer} requires '{first_constraint}', \
         but {second_requirer} requires '{second_constraint}'"
    )]
    VersionConflict {
        name: String,
        first_requirer: String,
        first_constraint: String,
        second_requirer: String,
        second_constraint: String,
    },

    #[error("'{a}' conflicts with '{b}' (conflict declared by {declared_by})")]
    PackageConflict {
        a: String,
        b: String,
        declared_by: String,
    },

    #[error("circular dependency among AUR packages: {}", cycle.join(" -> "))]
    DependencyCycle { cycle: Vec<String> },

    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Kahn's algorithm over `edges` (dependency -> dependents), restricted to
/// `nodes`. Deterministic: among ready nodes, lexicographically smallest
/// first. Returns dependency-first order, or the cycle that blocks one.
///
/// Only AUR packages go through this: official packages are installed by
/// pacman in a single transaction where order is pacman's problem (official
/// repos legitimately contain dependency cycles).
pub fn toposort(
    nodes: &BTreeSet<String>,
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<String>, ResolveError> {
    let mut indegree: BTreeMap<&str, usize> = nodes.iter().map(|n| (n.as_str(), 0)).collect();
    for (dep, dependents) in edges {
        if !nodes.contains(dep) {
            continue;
        }
        for d in dependents {
            if let Some(count) = indegree.get_mut(d.as_str()) {
                *count += 1;
            }
        }
    }

    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut order = Vec::with_capacity(nodes.len());

    while let Some(&next) = ready.iter().next() {
        ready.remove(next);
        order.push(next.to_owned());
        if let Some(dependents) = edges.get(next) {
            for d in dependents {
                if let Some(deg) = indegree.get_mut(d.as_str()) {
                    *deg -= 1;
                    if *deg == 0 {
                        ready.insert(d);
                    }
                }
            }
        }
    }

    if order.len() == nodes.len() {
        return Ok(order);
    }

    // Every leftover node still has an unsatisfied dependency among the
    // leftovers (that's the only way its indegree stays > 0), so following
    // "depends on" from any leftover must eventually revisit a node — the
    // cycle. Path reads left-to-right as "depends on".
    let done: BTreeSet<&str> = order.iter().map(String::as_str).collect();
    let leftover: BTreeSet<&str> = nodes
        .iter()
        .map(String::as_str)
        .filter(|n| !done.contains(n))
        .collect();
    let start = *leftover.iter().next().expect("leftover is non-empty");
    let mut path: Vec<&str> = vec![start];
    loop {
        let current = *path.last().expect("path never empty");
        // A dependency of `current` inside the leftover set: an edge
        // dep -> dependents with `current` among the dependents.
        let next = edges
            .iter()
            .find(|(dep, dependents)| {
                leftover.contains(dep.as_str()) && dependents.contains(current)
            })
            .map(|(dep, _)| dep.as_str())
            .expect("leftover node with no leftover dependency");
        if let Some(pos) = path.iter().position(|&n| n == next) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|s| (*s).to_owned()).collect();
            cycle.push(next.to_owned());
            return Err(ResolveError::DependencyCycle { cycle });
        }
        path.push(next);
    }
}
