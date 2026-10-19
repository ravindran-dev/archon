use strsim::jaro_winkler;

const SIMILARITY_THRESHOLD: f64 = 0.75;
const MAX_LENGTH_DELTA: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct PackageRef {
    pub name: String,
    pub source: String,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub pkg: PackageRef,
    pub similarity: f64,
}

pub fn suggest(query: &str, candidates: &[PackageRef], max_results: usize) -> Vec<Suggestion> {
    let query_len = query.chars().count();
    let query_lower = query.to_lowercase();

    let mut suggestions: Vec<Suggestion> = candidates
        .iter()
        .filter(|c| {
            let c_len = c.name.chars().count();
            let delta = c_len.abs_diff(query_len);
            delta <= MAX_LENGTH_DELTA
        })
        .filter_map(|c| {
            let sim = jaro_winkler(&query_lower, &c.name.to_lowercase());
            if sim >= SIMILARITY_THRESHOLD {
                Some(Suggestion {
                    pkg: c.clone(),
                    similarity: sim,
                })
            } else {
                None
            }
        })
        .collect();

    suggestions.sort_by(|a, b| {
        b.similarity
            .total_cmp(&a.similarity)
            .then_with(|| a.pkg.name.cmp(&b.pkg.name))
            .then_with(|| a.pkg.source.cmp(&b.pkg.source))
            .then_with(|| a.pkg.repo.cmp(&b.pkg.repo))
    });
    suggestions.truncate(max_results);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, source: &str, repo: Option<&str>) -> PackageRef {
        PackageRef {
            name: name.to_string(),
            source: source.to_string(),
            repo: repo.map(|s| s.to_string()),
        }
    }

    #[test]
    fn suggestion_ties_do_not_depend_on_index_iteration_order() {
        let a = cand("foo", "pacman", Some("extra"));
        let b = cand("foa", "pacman", Some("extra"));
        assert_eq!(
            suggest("fo", &[a.clone(), b.clone()], 5),
            suggest("fo", &[b, a], 5)
        );
    }

    #[test]
    fn test_suggest_obvious_typos() {
        let candidates = vec![
            cand("firefox", "pacman", Some("extra")),
            cand("fireball", "aur", None),
            cand("pyton", "aur", None),
            cand("python", "pacman", Some("core")),
        ];

        let suggs = suggest("fierfox", &candidates, 5);
        assert!(!suggs.is_empty());
        assert_eq!(suggs[0].pkg.name, "firefox");

        let suggs2 = suggest("pyton", &candidates, 5);
        assert!(!suggs2.is_empty());
        assert_eq!(suggs2[0].pkg.name, "pyton"); // Exact matches will be 1.0 similarity
        if suggs2.len() > 1 {
            assert_eq!(suggs2[1].pkg.name, "python");
        }
    }

    #[test]
    fn test_no_suggestions_for_wildly_different_strings() {
        let candidates = vec![
            cand("linux", "pacman", Some("core")),
            cand("gcc", "pacman", Some("core")),
        ];

        let suggs = suggest("fierfox", &candidates, 5);
        assert!(suggs.is_empty());
    }
}
