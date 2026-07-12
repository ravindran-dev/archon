//! Package discovery: backs both `archon search <term>` and the "did you
//! mean" suggestions offered when `install`/`plan` can't find an exact name.
//!
//! Exact-name lookup (what the resolver does) is the wrong tool for "what's
//! the package actually called" — a name like `cuttlefish` has no exact
//! match anywhere even though `cuttlefish-base-git` very much exists, and
//! exact lookup just reports "not found" with no way to discover that.
//! Substring search against official repo names (in-memory, no network)
//! and the AUR's own name search endpoint covers that gap.

use archon_aur::AurClient;
use archon_repo::OfficialRepos;

pub struct Hit {
    pub name: String,
    pub version: String,
    pub source: &'static str,
    pub description: Option<String>,
}

/// Both providers, combined: official repo hits first (repo precedence
/// order, deduped), then AUR hits. The AUR search is best-effort — offline
/// or AUR-down just means AUR hits are silently absent, not a second error
/// on top of whatever prompted the search in the first place.
pub fn find(official: &OfficialRepos, aur: &AurClient, term: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = official
        .search(term)
        .into_iter()
        .map(|p| Hit {
            name: p.name.clone(),
            version: p.version.to_string(),
            source: "official",
            description: p.description.clone(),
        })
        .collect();
    if let Ok(aur_hits) = aur.search_by_name(term) {
        hits.extend(aur_hits.into_iter().map(|p| Hit {
            name: p.name,
            version: p.version,
            source: "aur",
            description: p.description,
        }));
    }
    hits
}
