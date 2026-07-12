//! `archon search <term>` — substring match against package names in the
//! official repos and the AUR, so a name you're not sure of the exact
//! spelling for (`cuttlefish` when the real package is
//! `cuttlefish-base-git`) is actually discoverable instead of just failing.

use std::path::Path;

use anyhow::{Context, Result};
use archon_aur::AurClient;
use archon_repo::OfficialRepos;

use super::install;
use crate::color;
use crate::spinner::Spinner;

pub fn run(term: &str) -> Result<()> {
    let spinner = Spinner::start(format!("Searching for {term}"));
    let sync_dir = install::sync_dir();
    let repo_cache = install::cache_dir().join("sync-db.json");
    let official = match OfficialRepos::load_cached(Path::new(&sync_dir), archon_repo::DEFAULT_REPO_ORDER, &repo_cache)
        .with_context(|| format!("failed to load official repo databases from {sync_dir}"))
    {
        Ok(v) => v,
        Err(e) => {
            spinner.fail("search failed");
            return Err(e);
        }
    };
    let aur = AurClient::new(install::aur_url())?;

    let hits = crate::search::find(&official, &aur, term);
    if hits.is_empty() {
        spinner.fail(format!("no matches for {term}"));
        return Ok(());
    }
    spinner.success(format!("{} match(es)", hits.len()));

    println!();
    for hit in &hits {
        let tag = match hit.source {
            "official" => color::green("official"),
            _ => color::yellow("aur"),
        };
        print!("  {} {} {}", tag, color::bold(&hit.name), color::dim(&hit.version));
        if let Some(desc) = &hit.description {
            print!(" — {desc}");
        }
        println!();
    }
    Ok(())
}
