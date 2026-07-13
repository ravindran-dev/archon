//! `archon search <term>` — substring match against package names in the
//! official repos and the AUR, so a name you're not sure of the exact
//! spelling for (`cuttlefish` when the real package is
//! `cuttlefish-base-git`) is actually discoverable instead of just failing.
//!
//! Off Arch, there's no locally parsed package database to search
//! structurally (`archon_repo::OfficialRepos::search` is pacman-sync-db-
//! specific), so this just runs the native tool's own search and prints
//! its output as-is.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use archon_aur::AurClient;
use archon_distro::Distro;
use archon_repo::OfficialRepos;

use super::install;
use crate::color;
use crate::spinner::Spinner;

/// Shown by default; ranking (see `crate::search::rank`) means these are
/// the popular/relevant hits, so the long tail below them is noise for the
/// common "what's this package actually called" question — `--all` prints
/// everything for the times it isn't.
const DEFAULT_LIMIT: usize = 15;

pub fn run(term: &str, all: bool) -> Result<()> {
    let distro = Distro::detect();
    if !distro.is_arch() {
        return run_foreign_distro(&distro, term);
    }

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

    let shown = if all { hits.len() } else { hits.len().min(DEFAULT_LIMIT) };
    println!();
    for hit in &hits[..shown] {
        let tag = match hit.source {
            "official" => color::green("official"),
            _ => color::yellow("aur"),
        };
        print!("  {} {} {}", tag, color::bold(&hit.name), color::dim(&hit.version));
        if hit.votes > 0 {
            print!(" {}", color::dim(&format!("({} votes)", hit.votes)));
        }
        if let Some(desc) = &hit.description {
            print!(" — {desc}");
        }
        println!();
    }
    if shown < hits.len() {
        println!(
            "\n{}",
            color::dim(&format!(
                "…and {} more (sorted by popularity — run `archon search {term} --all` to see everything)",
                hits.len() - shown
            ))
        );
    }
    Ok(())
}

fn run_foreign_distro(distro: &Distro, term: &str) -> Result<()> {
    let Some(backend) = distro.backend() else {
        bail!(
            "no supported package manager detected for {distro}. archon currently supports \
             Arch (pacman), Debian/Ubuntu (apt), Fedora/RHEL (dnf), openSUSE (zypper), Alpine (apk), NixOS (nix-env), Void (xbps), and Gentoo (emerge)."
        );
    };
    let argv = backend.search_argv(term);
    println!("{}", color::dim(&format!("Detected {distro} — delegating to {}", backend.name())));
    println!("{}\n", color::dim(&argv.join(" ")));
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("failed to execute {} (is it installed and in PATH?)", backend.name()))?;
    if !status.success() {
        bail!("{} exited with {status}", backend.name());
    }
    Ok(())
}
