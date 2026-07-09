//! `archon build <dir> [--report]` — build a *local* PKGBUILD directory in
//! the sandbox, optionally under eBPF monitoring.
//!
//! This is the counterpart to `install`, which only knows how to fetch and
//! build packages that exist on the AUR. `build` points the same
//! sandbox+monitor machinery at a directory you already have on disk — your
//! own PKGBUILD, or one of the demo packages under `demo/`. It never
//! installs anything; it builds, reports, and leaves the package files in
//! place.
//!
//! It's the intended way to exercise the malicious demo
//! (`demo/archon-demo-evil`): a package that isn't on the AUR and that you
//! specifically do not want installed, only observed.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use archon_monitor::EventStore;
use archon_sandbox::SandboxRunner;

use crate::commands::install;

pub fn run(dir: &Path, report: bool) -> Result<()> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("build directory {} does not exist", dir.display()))?;
    if !dir.join("PKGBUILD").is_file() {
        bail!("no PKGBUILD found in {} — is this an AUR package directory?", dir.display());
    }
    if !SandboxRunner::bwrap_available() {
        bail!(
            "bubblewrap (bwrap) is not installed — required to build packages safely.\n\
             Install it with: sudo pacman -S bubblewrap"
        );
    }

    let pkg_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "local-package".into());

    if report {
        run_monitored(&dir, &pkg_name)
    } else {
        run_plain(&dir, &pkg_name)
    }
}

/// Unmonitored: just build in the sandbox and report where the package
/// landed. Runs as the current user (no privilege drop needed).
fn run_plain(dir: &Path, pkg_name: &str) -> Result<()> {
    let runner = SandboxRunner::new(dir).capture_output(true);
    install::build_in_sandbox(&runner, dir, pkg_name)?;
    report_outputs(dir, pkg_name)?;
    Ok(())
}

/// Monitored: load the eBPF monitor (needs root), then build under
/// observation and print the report. Reuses all of `install`'s monitored
/// pipeline so the two paths can't drift apart.
fn run_monitored(dir: &Path, pkg_name: &str) -> Result<()> {
    let privileges = install::require_monitoring_privileges()?;
    let mut monitor = install::load_monitor()?;

    // The directory must be writable by the dropped-to build user, just
    // like a freshly cloned AUR package would be.
    install::ensure_owned_recursive(dir, privileges.uid, privileges.gid)
        .with_context(|| format!("failed to chown {} to the build user", dir.display()))?;

    let user_home = install::home_dir_for_uid(privileges.uid)?;
    let build_root = install::build_dir_under(&user_home);
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create {}", build_root.display()))?;
    // See install.rs's build_aur_packages_monitored for why the parent
    // (the whole archon cache root, not just build/) needs this too.
    if let Some(cache_root) = build_root.parent() {
        install::ensure_owned(cache_root, privileges.uid, privileges.gid)?;
    }
    install::ensure_owned(&build_root, privileges.uid, privileges.gid)?;
    let store = EventStore::open(&build_root.join("monitor.db"))
        .context("failed to open event store")?;

    let report = install::run_monitored_build(&mut monitor, &store, dir, pkg_name, &privileges)?;
    crate::ui::print_report(&report);

    if report.is_clean() {
        report_outputs(dir, pkg_name)?;
    } else {
        println!("{}", crate::color::red("not installed — this is `build`, which never installs anyway"));
    }
    Ok(())
}

fn report_outputs(dir: &Path, pkg_name: &str) -> Result<()> {
    let files = install::find_package_files(dir, pkg_name)?;
    // The package() step may legitimately produce a differently-named
    // artifact; a miss here isn't an error, just nothing to point at.
    for f in files {
        println!("  {} {}", crate::color::dim("→"), f.display());
    }
    Ok(())
}

/// Convenience for `main`: canonicalize a user-supplied path argument.
pub fn parse_dir(arg: &str) -> PathBuf {
    PathBuf::from(arg)
}
