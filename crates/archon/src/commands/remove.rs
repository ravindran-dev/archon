//! `archon remove <packages>` — uninstall packages.
//!
//! archon does no dependency-removal computation of its own: `pacman -Rs`
//! already does exactly that (removing the named packages plus any
//! dependency that's no longer required by anything else), correctly and
//! safely, and reimplementing it would be exactly the kind of "our
//! value-add is the resolver and the security monitor, not reinventing
//! pacman" scope creep the project deliberately avoids. archon's part is
//! just the pre-flight check (are these actually installed? by which
//! version?) and the same plan/confirm UX `install` uses, so removing
//! feels consistent with installing rather than bolted on.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use archon_distro::Distro;

use crate::commands::install;
use crate::spinner::Spinner;
use crate::{color, ui};

/// Non-Arch path: no local package database archon knows how to read (the
/// pre-flight "is this actually installed, at what version" check below is
/// pacman-specific), so this delegates straight to the native tool and
/// lets it say so if a name isn't installed — same reasoning as
/// `install::run_foreign_distro`.
fn run_foreign_distro(distro: &Distro, packages: &[String], dry_run: bool, yes: bool) -> Result<()> {
    let Some(backend) = distro.backend() else {
        bail!(
            "no supported package manager detected for {distro}. archon currently supports \
             Arch (pacman), Debian/Ubuntu (apt), Fedora/RHEL (dnf), openSUSE (zypper), Alpine (apk), NixOS (nix-env), Void (xbps), and Gentoo (emerge)."
        );
    };
    let argv = backend.remove_argv(packages, yes);
    println!("{}", color::dim(&format!("Detected {distro} — delegating to {}", backend.name())));
    install::run_native_tool(backend.as_ref(), &argv, dry_run)
}

pub fn run(packages: &[String], dry_run: bool, yes: bool) -> Result<()> {
    let distro = Distro::detect();
    if !distro.is_arch() {
        return run_foreign_distro(&distro, packages, dry_run, yes);
    }

    let spinner = Spinner::start(format!("Checking {}", packages.join(", ")));
    let local_dir = install::local_dir();
    let local_db = match archon_repo::LocalDb::load_cached(
        Path::new(&local_dir),
        &install::cache_dir().join("local-db.json"),
    )
    .with_context(|| format!("failed to read the local package database at {local_dir}"))
    {
        Ok(v) => v,
        Err(e) => {
            spinner.fail("check failed");
            return Err(e);
        }
    };

    let mut missing = Vec::new();
    let mut found = Vec::new();
    for name in packages {
        match local_db.version_of(name) {
            Some(version) => found.push((name.clone(), version.to_string())),
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        spinner.fail("not installed");
        bail!(
            "not installed, nothing to remove: {}\n(pacman -R would refuse this exact set too — \
             it's all-or-nothing, same as archon here)",
            missing.join(", ")
        );
    }
    spinner.success(format!("{} package(s) installed, ready to remove", found.len()));

    println!("{} {}\n", color::bold("▶ Remove plan —"), color::dim(&format!("{} package(s)", found.len())));
    let name_width = found.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, version) in &found {
        println!("  {:<name_width$}  {}", name, color::dim(version), name_width = name_width);
    }
    println!(
        "\n{}",
        color::dim("pacman -Rs also removes any now-orphaned dependencies — nothing else on your system.")
    );

    if dry_run {
        return Ok(());
    }
    if !yes && !ui::confirm("\nProceed with removal?")? {
        println!("aborted — nothing was changed.");
        return Ok(());
    }

    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    println!("{}", color::dim(&format!("pacman -Rs {}", names.join(" "))));
    let status = Command::new("sudo")
        .arg("pacman")
        .arg("-Rs")
        .args(&names)
        .status()
        .context("failed to execute pacman (is it installed and in PATH?)")?;
    if !status.success() {
        bail!("pacman exited with {status}");
    }
    Ok(())
}
