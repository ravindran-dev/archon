//! Package discovery: backs both `archon search <term>` and the "did you
//! mean" suggestions offered when `install`/`plan` can't find an exact name.
//!
//! Exact-name lookup (what the resolver does) is the wrong tool for "what's
//! the package actually called" — a name like `cuttlefish` has no exact
//! package by that name at all (the real ones are `cuttlefish-base-git`,
//! `cuttlefish-user-git`, ...), so a user has no way to discover them
//! without something that matches on substrings. Substring search against
//! official repo names (in-memory, no network) and the AUR's own name
//! search endpoint covers that gap.
//!
//! Results are *ranked*, not alphabetical: a search like `chrome` matches
//! 70+ packages, and dumping them A→Z buries `google-chrome` (2000+ votes)
//! under `chrome-extension-ocrs-git` (0 votes). Ranking uses the AUR's own
//! `Popularity` metric — a recency-weighted vote count, the closest
//! available proxy for "what users currently actually install" — with an
//! exact-name match always pinned first (searching `python` must surface
//! official `python` above any popularity contest).

use archon_aur::AurClient;
use archon_repo::OfficialRepos;

pub struct Hit {
    pub name: String,
    pub version: String,
    pub source: &'static str,
    pub description: Option<String>,
    /// AUR all-time votes; 0 for official packages (no such stat exists
    /// in sync databases).
    pub votes: u64,
    /// AUR recency-weighted popularity; 0.0 for official packages.
    pub popularity: f64,
}

/// Both providers, combined and ranked (see [`rank`]). The AUR search is
/// best-effort — offline or AUR-down just means AUR hits are silently
/// absent, not a second error on top of whatever prompted the search in
/// the first place.
pub fn find(official: &OfficialRepos, aur: &AurClient, term: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = official
        .search(term)
        .into_iter()
        .map(|p| Hit {
            name: p.name.clone(),
            version: p.version.to_string(),
            source: "official",
            description: p.description.clone(),
            votes: 0,
            popularity: 0.0,
        })
        .collect();
    if let Ok(aur_hits) = aur.search_by_name(term) {
        hits.extend(aur_hits.into_iter().map(|p| Hit {
            name: p.name,
            version: p.version,
            source: "aur",
            description: p.description,
            votes: p.num_votes,
            popularity: p.popularity,
        }));
    }
    rank(&mut hits, term);
    hits
}

/// Orders hits by: exact name match first, then popularity descending,
/// then official before AUR, then prefix match before plain substring,
/// then name.
///
/// Two orderings here are deliberate and earned their position the hard
/// way:
///
/// - Exact match outranks popularity: searching `python` must surface
///   official `python` above any popularity contest.
/// - Popularity outranks *prefix* matching. A prefix tier feels natural
///   ("starts with what I typed = more relevant") but re-creates the exact
///   live complaint this ranking was built to fix: for `chrome`,
///   `google-chrome` (popularity ~30, the thing virtually everyone
///   searching "chrome" wants) is only a *substring* match, and a prefix
///   tier buries it under zero-vote `chrome-extension-*` packages again.
///   Prefix is a tie-break among the equally-unpopular, nothing more.
///
/// The official-before-AUR tie-break matters exactly at popularity 0.0: an
/// official package (which has no popularity stat at all) shouldn't lose
/// to an abandoned zero-vote AUR package, but *should* lose to a genuinely
/// popular one — curation is a tie-break signal, not a trump card
/// (official `vlc-plugin-chromecast` has no business above `google-chrome`).
fn rank(hits: &mut [Hit], term: &str) {
    let term = term.to_lowercase();
    hits.sort_by(|a, b| {
        let exact = |h: &Hit| h.name.to_lowercase() != term;
        let prefix = |h: &Hit| !h.name.to_lowercase().starts_with(&term);
        exact(a)
            .cmp(&exact(b))
            .then_with(|| b.popularity.total_cmp(&a.popularity))
            .then_with(|| (a.source != "official").cmp(&(b.source != "official")))
            .then_with(|| prefix(a).cmp(&prefix(b)))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, source: &'static str, popularity: f64) -> Hit {
        Hit {
            name: name.into(),
            version: "1.0-1".into(),
            source,
            description: None,
            votes: 0,
            popularity,
        }
    }

    fn ranked_names(mut hits: Vec<Hit>, term: &str) -> Vec<String> {
        rank(&mut hits, term);
        hits.into_iter().map(|h| h.name).collect()
    }

    #[test]
    fn popularity_beats_alphabetical_order() {
        // The live complaint this ranking exists to fix: alphabetical put
        // a zero-vote extension above the browser everyone installs.
        let names = ranked_names(
            vec![
                hit("chrome-extension-ocrs-git", "aur", 0.0),
                hit("google-chrome", "aur", 30.0),
                hit("chromedriver", "aur", 2.5),
            ],
            "chrome",
        );
        assert_eq!(
            names,
            ["google-chrome", "chromedriver", "chrome-extension-ocrs-git"]
        );
    }

    #[test]
    fn exact_match_pins_first_regardless_of_popularity() {
        let names = ranked_names(
            vec![
                hit("python-somethingpopular", "aur", 99.0),
                hit("python", "official", 0.0),
            ],
            "python",
        );
        assert_eq!(names[0], "python");
    }

    #[test]
    fn prefix_match_beats_plain_substring_within_same_popularity() {
        let names = ranked_names(
            vec![
                hit("libchrome-tools", "aur", 0.0),
                hit("chrome-tools", "aur", 0.0),
            ],
            "chrome",
        );
        assert_eq!(names, ["chrome-tools", "libchrome-tools"]);
    }

    #[test]
    fn official_wins_ties_but_not_against_real_popularity() {
        let names = ranked_names(
            vec![
                hit("chrome-junk-git", "aur", 0.0),
                hit("vlc-plugin-chromecast", "official", 0.0),
                hit("google-chrome", "aur", 30.0),
            ],
            "chrome",
        );
        assert_eq!(
            names,
            ["google-chrome", "vlc-plugin-chromecast", "chrome-junk-git"]
        );
    }

    #[test]
    fn ranking_is_case_insensitive() {
        let names = ranked_names(
            vec![
                hit("other-Firefox-thing", "aur", 5.0),
                hit("Firefox", "aur", 0.0),
            ],
            "firefox",
        );
        assert_eq!(
            names[0], "Firefox",
            "exact match must be found despite case differences"
        );
    }
}
