//! Iterative backtracking with branch-local state and shared provider caches.
use std::collections::{BTreeSet, HashMap, VecDeque};

use archon_types::{Dependency, PackageMeta, PackageSource, VersionBackend, VersionCompareError};

use crate::{
    InstallPlan, ResolveError, Resolver, TARGET, build_plan, check_conflicts, note_requirer,
    package_conflict, try_satisfy,
};

#[derive(Clone)]
enum Task {
    Need(Dependency, String),
    Select(Dependency, String, Box<PackageMeta>),
}

#[derive(Clone, Default)]
struct State {
    queue: VecDeque<Task>,
    resolved: HashMap<String, PackageMeta>,
    requirers: HashMap<String, Vec<(String, String)>>,
}

#[derive(Default)]
struct Cache {
    exact: HashMap<(usize, String), Vec<PackageMeta>>,
    provides: HashMap<(usize, String), Vec<PackageMeta>>,
}

fn validate(meta: &PackageMeta, backend: VersionBackend) -> Result<(), ResolveError> {
    let source_backend = match meta.source {
        PackageSource::Apt => VersionBackend::Dpkg,
        _ => VersionBackend::Alpm,
    };
    for expected in [source_backend, backend] {
        if meta.version.backend != expected {
            return Err(VersionCompareError {
                left: meta.version.backend,
                right: expected,
            }
            .into());
        }
    }
    let mut dependencies: Vec<_> = meta
        .depends
        .iter()
        .chain(&meta.make_depends)
        .chain(&meta.provides)
        .chain(&meta.conflicts)
        .collect();
    while let Some(dependency) = dependencies.pop() {
        if let Some(constraint) = &dependency.constraint
            && constraint.version.backend != backend
        {
            return Err(VersionCompareError {
                left: constraint.version.backend,
                right: backend,
            }
            .into());
        }
        dependencies.extend(&dependency.alternatives);
    }
    Ok(())
}

impl Cache {
    fn candidates(
        &mut self,
        resolver: &Resolver<'_>,
        dep: &Dependency,
        state: &State,
    ) -> Result<Vec<PackageMeta>, ResolveError> {
        if let Some(cached) = self.exact.get(&(0, dep.name.clone())) {
            let mut matches = Vec::new();
            for meta in cached {
                if meta.satisfies(dep)? {
                    matches.push(meta.clone());
                }
            }
            if !matches.is_empty() {
                return Ok(matches);
            }
        }
        // Prefetch the current BFS frontier. Shared caches survive backtracking,
        // while selected packages, pending edges and requirers never do.
        let mut names: BTreeSet<String> = state
            .queue
            .iter()
            .filter_map(|task| match task {
                Task::Need(d, _) if !state.resolved.contains_key(&d.name) => Some(d.name.clone()),
                _ => None,
            })
            .collect();
        names.insert(dep.name.clone());
        for (tier, provider) in resolver.providers.iter().enumerate() {
            let misses: Vec<_> = names
                .iter()
                .filter(|name| !self.exact.contains_key(&(tier, (*name).clone())))
                .cloned()
                .collect();
            if !misses.is_empty() {
                let results = provider.get(&misses)?;
                for name in &misses {
                    self.exact.insert((tier, name.clone()), Vec::new());
                }
                for meta in results {
                    validate(&meta, resolver.backend)?;
                    if let Some(entries) = self.exact.get_mut(&(tier, meta.name.clone())) {
                        entries.push(meta);
                    }
                }
            }
            let exact = self
                .exact
                .get(&(tier, dep.name.clone()))
                .cloned()
                .unwrap_or_default();
            let mut matches = Vec::new();
            for meta in &exact {
                if meta.satisfies(dep)? {
                    matches.push(meta.clone());
                }
            }
            if !matches.is_empty() {
                return Ok(matches);
            }

            // Provider searches are individual network requests on the AUR.
            // Only search the dependency being selected; speculative searches
            // for the whole frontier delayed exact-name matches needlessly.
            let key = (tier, dep.name.clone());
            if let std::collections::hash_map::Entry::Vacant(entry) = self.provides.entry(key) {
                let candidates = provider.find_providers(&dep.name)?;
                for candidate in &candidates {
                    validate(candidate, resolver.backend)?;
                }
                entry.insert(candidates);
            }
            for meta in self
                .provides
                .get(&(tier, dep.name.clone()))
                .into_iter()
                .flatten()
            {
                if meta.satisfies(dep)? {
                    matches.push(meta.clone());
                }
            }
            if !matches.is_empty() {
                matches.sort_by(|a, b| {
                    let installed = |m: &PackageMeta| {
                        resolver
                            .prefer_installed
                            .is_some_and(|set| set.contains(&m.name))
                    };
                    installed(b)
                        .cmp(&installed(a))
                        .then_with(|| a.name.cmp(&b.name))
                });
                matches.dedup_by(|a, b| a == b);
                return Ok(matches);
            }
            // Exact names obey repository precedence even on a version mismatch.
            if let Some(meta) = exact.first() {
                return Err(ResolveError::NoSatisfyingVersion {
                    name: meta.name.clone(),
                    dep: dep.to_string(),
                    required_by: String::new(),
                    available: meta.version.to_string(),
                });
            }
            names.retain(|name| {
                name == &dep.name || {
                    let key = (tier, name.clone());
                    self.exact.get(&key).is_none_or(Vec::is_empty)
                        && self.provides.get(&key).is_none_or(Vec::is_empty)
                }
            });
        }
        Err(ResolveError::NotFound {
            name: dep.name.clone(),
            required_by: String::new(),
        })
    }
}

fn select(
    state: &mut State,
    dep: Dependency,
    requirer: String,
    meta: PackageMeta,
) -> Result<(), ResolveError> {
    if let Some(existing) = state.resolved.get(&meta.name) {
        if existing == &meta {
            note_requirer(&mut state.requirers, &meta.name, &requirer, &dep);
            return Ok(());
        }
        let (first_requirer, first_constraint) = state
            .requirers
            .get(&meta.name)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or((TARGET.into(), meta.name.clone()));
        return Err(ResolveError::VersionConflict {
            name: meta.name,
            first_requirer,
            first_constraint,
            second_requirer: requirer,
            second_constraint: dep.to_string(),
        });
    }
    let mut existing: Vec<_> = state.resolved.values().collect();
    existing.sort_by(|a, b| a.name.cmp(&b.name));
    for other in existing {
        if let Some(error) = package_conflict(&meta, other)? {
            return Err(error);
        }
    }
    note_requirer(&mut state.requirers, &meta.name, &requirer, &dep);
    for edge in &meta.depends {
        state
            .queue
            .push_back(Task::Need(edge.clone(), meta.name.clone()));
    }
    if meta.is_aur() {
        for edge in &meta.make_depends {
            state
                .queue
                .push_back(Task::Need(edge.clone(), meta.name.clone()));
        }
    }
    state.resolved.insert(meta.name.clone(), meta);
    Ok(())
}

pub(super) fn resolve(
    resolver: &Resolver<'_>,
    targets: &[String],
) -> Result<InstallPlan, ResolveError> {
    let mut state = State::default();
    for target in targets {
        let dep = Dependency::parse_with_backend(target, resolver.backend).map_err(|_| {
            ResolveError::NotFound {
                name: target.clone(),
                required_by: TARGET.into(),
            }
        })?;
        state.queue.push_back(Task::Need(dep, TARGET.into()));
    }
    let mut branches = Vec::new();
    let mut cache = Cache::default();
    let mut steps = 0;
    loop {
        let result = (|| {
            while let Some(task) = state.queue.pop_front() {
                steps += 1;
                if steps > resolver.search_limit {
                    return Err(ResolveError::SearchLimit { steps });
                }
                match task {
                    Task::Need(mut dep, requirer) => {
                        if let Some(mut fallback) = dep.alternatives.first().cloned() {
                            fallback
                                .alternatives
                                .extend_from_slice(&dep.alternatives[1..]);
                            let mut alternative = state.clone();
                            alternative
                                .queue
                                .push_front(Task::Need(fallback, requirer.clone()));
                            branches.push(alternative);
                            dep.alternatives.clear();
                        }
                        let mut existing_choices = Vec::new();
                        if let Some(exact) = state.resolved.get(&dep.name)
                            && exact.satisfies(&dep)?
                        {
                            existing_choices.push(exact.clone());
                        } else {
                            for meta in state.resolved.values() {
                                if meta.satisfies(&dep)? {
                                    existing_choices.push(meta.clone());
                                }
                            }
                            existing_choices.sort_by(|a, b| a.name.cmp(&b.name));
                        }
                        let candidates = if existing_choices.is_empty() {
                            if try_satisfy(&state.resolved, &mut state.requirers, &dep, &requirer)?
                            {
                                continue;
                            }
                            cache.candidates(resolver, &dep, &state).map_err(
                                |error| match error {
                                    ResolveError::NotFound { name, .. } => ResolveError::NotFound {
                                        name,
                                        required_by: requirer.clone(),
                                    },
                                    ResolveError::NoSatisfyingVersion {
                                        name,
                                        dep,
                                        available,
                                        ..
                                    } => ResolveError::NoSatisfyingVersion {
                                        name,
                                        dep,
                                        available,
                                        required_by: requirer.clone(),
                                    },
                                    other => other,
                                },
                            )?
                        } else {
                            existing_choices
                        };
                        for meta in candidates.iter().skip(1).rev() {
                            let mut alternative = state.clone();
                            alternative.queue.push_front(Task::Select(
                                dep.clone(),
                                requirer.clone(),
                                Box::new(meta.clone()),
                            ));
                            branches.push(alternative);
                        }
                        if let Some(meta) = candidates.into_iter().next() {
                            select(&mut state, dep, requirer, meta)?;
                        }
                    }
                    Task::Select(dep, requirer, meta) => select(&mut state, dep, requirer, *meta)?,
                }
            }
            check_conflicts(&state.resolved)?;
            build_plan(state.resolved.clone(), &state.requirers)
        })();
        match result {
            Ok(plan) => return Ok(plan),
            Err(
                error @ (ResolveError::Provider(_)
                | ResolveError::Version(_)
                | ResolveError::SearchLimit { .. }),
            ) => return Err(error),
            Err(error) => match branches.pop() {
                Some(next) => state = next,
                None => return Err(error),
            },
        }
    }
}
