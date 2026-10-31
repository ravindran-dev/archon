//! `archon install` / `archon plan`.
//!
//! archon's own resolver, AUR client, and sandboxed/eBPF-monitored build
//! pipeline are Arch-specific — pacman's sync db format, the AUR, and
//! PKGBUILD/makepkg have no equivalents elsewhere. [`archon_distro::Distro
//! ::detect`] runs first; off Arch, everything below is skipped in favor of
//! [`run_foreign_distro`], which delegates straight to the host's native
//! package manager (see [`archon_distro`]'s crate doc comment for why).
//!
//! The Arch flow:
//! 1. Load providers: [`OfficialRepos`] from `/var/lib/pacman/sync`
//!    (read-only), [`AurClient`] against aur.archlinux.org.
//! 2. [`Resolver::resolve`] -> [`InstallPlan`] (or a precise error).
//! 3. Print the plan, pacman-style: official packages, then AUR packages
//!    in build order, with versions and what pulled each one in.
//! 4. `--dry-run` stops here — guaranteed no side effects past this point.
//!    Otherwise prompt `[y/N]` (unless `--yes`), then:
//!    - with --refresh: refresh/upgrade with `sudo pacman -Syu`, reload metadata and resolve again
//!    - official: `sudo pacman -S --needed <pkgs>` (pacman owns its own
//!      confirmation and privilege handling from here)
//!    - AUR: `git clone` each package's AUR repo, then build it inside the
//!      bubblewrap sandbox ([`archon_sandbox`]).
//!
//! Without `--report`, AUR packages are built in bubblewrap and installed
//! via pacman in dependency order. eBPF monitoring is optional; its absence
//! is stated before confirmation.
//!
//! With `--report`, archon additionally monitors the build with eBPF and
//! only offers to install the result if nothing was flagged. That monitor
//! needs root to load, which conflicts with `makepkg` refusing outright to
//! run as root — see [`run_monitored`] for how the two are reconciled.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use archon_aur::AurClient;
use archon_distro::Distro;
use archon_monitor::{BuildReport, EventStore, FlaggedEvent, Monitor, Policy};
use archon_repo::OfficialRepos;
use archon_resolver::{PlannedPackage, ResolveError, Resolver};
use archon_sandbox::{BuildStage, SandboxRunner};
use archon_types::PackageProvider;

use super::subprocess::{self, OutputCapture};
use crate::archive::{ArchiveInstaller, PackageArchive, RootInstaller};
use crate::spinner::Spinner;
use crate::{color, srcinfo, ui};

const SYNC_DIR: &str = "/var/lib/pacman/sync";
const LOCAL_DIR: &str = "/var/lib/pacman/local";

/// Overridable so integration tests can point at fixture data instead of
/// the real system — production runs always get the real path/URL since
/// these env vars are never set outside the test suite.
pub(crate) fn sync_dir() -> String {
    std::env::var("ARCHON_SYNC_DIR").unwrap_or_else(|_| SYNC_DIR.to_string())
}

pub(crate) fn aur_url() -> String {
    std::env::var("ARCHON_AUR_URL").unwrap_or_else(|_| archon_aur::DEFAULT_AUR_URL.to_string())
}

pub(crate) fn local_dir() -> String {
    std::env::var("ARCHON_LOCAL_DIR").unwrap_or_else(|_| LOCAL_DIR.to_string())
}

/// Where cached, already-parsed sync/local db snapshots live — see
/// `archon_repo::cache`'s doc comment for why this matters (parsing both
/// from scratch on every single command measured ~925ms combined on a
/// real system). Overridable for the same reason `sync_dir`/`local_dir`
/// are: so tests never share state with, or depend on write access to,
/// the real user's cache directory.
pub(crate) fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARCHON_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|base| base.join("archon"))
        .unwrap_or_else(|| PathBuf::from("/tmp/archon-cache"))
}

/// Match enabled repositories and precedence to pacman's effective config.
/// Fixture databases retain an isolated, deterministic order.
pub(crate) fn load_official_repos(cached: bool) -> Result<OfficialRepos> {
    let directory = sync_dir();
    let order = if let Ok(order) = std::env::var("ARCHON_REPO_ORDER") {
        order.split(':').map(str::to_string).collect::<Vec<_>>()
    } else if std::env::var_os("ARCHON_SYNC_DIR").is_some() {
        // Hermetic test fixtures are intentionally independent of host config.
        return if cached && unsafe { libc::geteuid() } != 0 {
            Ok(OfficialRepos::load_cached(
                Path::new(&directory),
                archon_repo::DEFAULT_REPO_ORDER,
                &cache_dir().join("sync-db.json"),
            )?)
        } else {
            Ok(OfficialRepos::load(
                Path::new(&directory),
                archon_repo::DEFAULT_REPO_ORDER,
            )?)
        };
    } else {
        let output = Command::new("pacman-conf")
            .arg("--repo-list")
            .output()
            .context("cannot read enabled repositories with pacman-conf --repo-list")?;
        if !output.status.success() {
            bail!(
                "pacman-conf failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout)?
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    if order.is_empty() {
        bail!("pacman has no enabled repositories");
    }
    let names = order.iter().map(String::as_str).collect::<Vec<_>>();
    let cache = cache_dir().join("sync-db.json");
    Ok(OfficialRepos::load_configured(
        Path::new(&directory),
        &names,
        (cached && unsafe { libc::geteuid() } != 0).then_some(cache.as_path()),
    )?)
}

/// Best-effort: used only to keep the plan display honest about what's
/// already on the system, never for resolution itself, so any failure
/// (missing dir, permissions) just falls back to showing everything —
/// the same as if this lookup didn't exist at all.
fn load_local_db() -> Option<archon_repo::LocalDb> {
    if unsafe { libc::geteuid() } == 0 {
        archon_repo::LocalDb::load(Path::new(&local_dir())).ok()
    } else {
        archon_repo::LocalDb::load_cached(
            Path::new(&local_dir()),
            &cache_dir().join("local-db.json"),
        )
        .ok()
    }
}

/// Non-Arch path. There's no archon-computed dependency graph here to show
/// a plan for, so unlike the Arch path there's no separate archon-side
/// `[y/N]` prompt either — the native tool's own transaction summary and
/// confirmation *is* the confirmation, exactly as if you'd run it
/// yourself. `--yes` still means something: it maps to the native tool's
/// own assume-yes flag, the same way `--yes` on Arch skips archon's prompt
/// without touching pacman's.
fn run_foreign_distro(
    distro: &Distro,
    packages: &[String],
    dry_run: bool,
    yes: bool,
    report: bool,
) -> Result<()> {
    if report {
        bail!(
            "--report is only available on Arch for AUR builds; refusing to continue without eBPF monitoring"
        );
    }
    let Some(backend) = distro.backend() else {
        bail!(
            "no supported package manager detected for {distro}. archon currently supports \
             Arch (pacman + AUR), Debian/Ubuntu (apt), Fedora/RHEL (dnf), openSUSE (zypper), \
             Alpine (apk), NixOS (nix-env), Void (xbps), and Gentoo (emerge) — if you'd like \
             {distro} supported, a backend is just an `archon_distro::Backend` impl (see \
             crates/archon-distro/src/backend.rs)."
        );
    };
    super::native::ensure_mutable_host(distro)?;
    let argv = backend.install_argv(packages, yes);
    println!(
        "{}",
        color::dim(&format!(
            "Detected {distro} — delegating to {} (its own dependency resolution and confirmation apply, not archon's)",
            backend.name()
        ))
    );
    run_native_tool(backend.as_ref(), &argv, dry_run)
}

/// Prints and (unless `dry_run`) executes a native package manager's argv,
/// under `sudo` only when the backend says it needs root — `nix-env`
/// notably does not: it installs into the *invoking user's* profile, and
/// sudo would silently redirect the install into root's profile instead.
pub(crate) fn run_native_tool(
    backend: &dyn archon_distro::Backend,
    argv: &[String],
    dry_run: bool,
) -> Result<()> {
    let display = if backend.needs_root() {
        format!("sudo {}", argv.join(" "))
    } else {
        argv.join(" ")
    };
    println!("{}", color::dim(&display));
    if dry_run {
        return Ok(());
    }
    let mut cmd = if backend.needs_root() {
        let mut c = Command::new("sudo");
        c.args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    let status = cmd.status().with_context(|| {
        format!(
            "failed to execute {} (is it installed and in PATH?)",
            backend.name()
        )
    })?;
    if !status.success() {
        bail!("{} exited with {status}", backend.name());
    }
    Ok(())
}

pub fn run(
    packages: &[String],
    dry_run: bool,
    yes: bool,
    report: bool,
    refresh: bool,
) -> Result<()> {
    validate_package_names(packages)?;
    if refresh && !dry_run && Distro::detect().is_arch() {
        if report {
            require_monitoring_privileges()?;
        }
        println!(
            "--refresh will refresh repositories and perform a full system upgrade with pacman -Syu before resolving the install plan."
        );
        if !yes && !ui::confirm("Refresh repositories and upgrade the system first?")? {
            println!("aborted — nothing was changed.");
            return Ok(());
        }
        synchronize_system()?;
    }
    run_with_history(
        packages,
        dry_run,
        yes,
        report,
        refresh,
        &std::collections::HashMap::new(),
    )
}

fn run_with_history(
    packages: &[String],
    dry_run: bool,
    yes: bool,
    report: bool,
    refresh: bool,
    correction_history: &std::collections::HashMap<usize, Vec<String>>,
) -> Result<()> {
    validate_package_names(packages)?;
    let distro = Distro::detect();
    if !distro.is_arch() {
        if refresh {
            bail!(
                "install --refresh is Arch-specific; use `archon upgrade` to update this distribution"
            );
        }
        return run_foreign_distro(&distro, packages, dry_run, yes, report);
    }

    let spinner = Spinner::start(format!("Resolving {}", packages.join(", ")));
    let sync_dir = sync_dir();
    let official = match load_official_repos(!refresh).with_context(|| {
        format!(
            "failed to load official repo databases from {sync_dir} \
                 (run `sudo pacman -Syu` if you've never synced)"
        )
    }) {
        Ok(v) => v,
        Err(e) => {
            spinner.fail("resolution failed");
            return Err(e);
        }
    };
    let aur = AurClient::new(aur_url())?;
    let local_db = if refresh && !dry_run {
        Some(
            archon_repo::LocalDb::load(Path::new(&local_dir()))
                .context("cannot reload installed packages after refresh")?,
        )
    } else {
        load_local_db()
    };
    let installed_names: std::collections::HashSet<String> = local_db
        .as_ref()
        .map(|db| db.names().map(str::to_string).collect())
        .unwrap_or_default();

    let providers: Vec<&dyn PackageProvider> = vec![&official, &aur];
    let resolve_result = Resolver::new(providers)
        .prefer_installed(&installed_names)
        .resolve(packages);
    let plan = match resolve_result {
        Ok(v) => v,
        Err(ResolveError::NotFound { name, required_by }) => {
            spinner.fail("resolution failed");
            if required_by == archon_resolver::TARGET {
                let mut pos = packages.len();
                if let Some(p) = packages.iter().position(|x| *x == name) {
                    pos = p;
                }

                let mut current_chain = crate::correction::chain(
                    correction_history
                        .get(&pos)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    &name,
                )
                .map_err(anyhow::Error::msg)?;

                if let Some(corrected) = handle_typo_correction(&official, &aur, &name)? {
                    current_chain.push(corrected.clone());

                    let mut new_history = correction_history.clone();
                    new_history.insert(pos, current_chain);

                    let mut new_packages = packages.to_vec();
                    if pos < new_packages.len() {
                        new_packages[pos] = corrected;
                    }
                    return run_with_history(
                        &new_packages,
                        dry_run,
                        yes,
                        report,
                        refresh,
                        &new_history,
                    );
                }
            }
            return Err(not_found_with_suggestions(
                &official,
                &aur,
                &name,
                &required_by,
            ));
        }
        Err(e) => {
            spinner.fail("resolution failed");
            return Err(anyhow::Error::new(e).context("dependency resolution failed"));
        }
    };

    if plan.is_empty() {
        spinner.success("nothing to do — already resolved");
        return Ok(());
    }
    spinner.success(format!("resolved {} package(s)", plan.package_count()));

    ui::print_plan(&plan, local_db.as_ref());

    if dry_run {
        if refresh {
            println!(
                "With --refresh, installation will first run pacman -Syu and recheck this locally cached plan."
            );
        }
        return Ok(());
    }
    if plan.official.iter().all(|pkg| {
        local_db
            .as_ref()
            .is_some_and(|db| db.satisfies(&pkg.meta.name, &pkg.meta.version))
    }) && pending_aur(&plan.aur, local_db.as_ref()).is_empty()
    {
        println!(
            "{} Requested packages are already installed and up to date: {}",
            color::green("✓"),
            packages.join(", ")
        );
        return Ok(());
    }

    if report {
        require_monitoring_privileges()?;
    }
    preflight_aur(&pending_aur(&plan.aur, local_db.as_ref()), report)?;

    if !report && !plan.aur.is_empty() {
        println!(
            "{} {}",
            color::yellow("!"),
            color::dim(
                "AUR builds are sandboxed; eBPF monitoring is off (use --report to enable)."
            )
        );
    }

    if !yes && !ui::confirm("\nProceed with installation?")? {
        if refresh {
            println!("installation cancelled; the system upgrade has already completed.");
        } else {
            println!("aborted — nothing was changed.");
        }
        return Ok(());
    }

    if !plan.official.is_empty() {
        install_official(&plan.official, local_db.as_ref())?;
    }

    let pending = pending_aur(&plan.aur, local_db.as_ref());
    preflight_aur(&pending, report)?;
    if !pending.is_empty() {
        if report {
            build_aur_packages_monitored(&aur, &pending)?;
        } else {
            build_aur_packages(&aur, &pending)?;
        }
    } else if !plan.aur.is_empty() {
        println!("all resolved AUR packages are installed and up to date");
    }
    println!(
        "{} Requested packages are installed: {}",
        color::green("✓"),
        packages.join(", ")
    );
    Ok(())
}

fn vcs_package(pkg: &PlannedPackage) -> bool {
    ["-git", "-svn", "-hg", "-bzr"]
        .iter()
        .any(|suffix| pkg.meta.build_base().ends_with(suffix))
}

fn pending_aur(
    pkgs: &[PlannedPackage],
    local: Option<&archon_repo::LocalDb>,
) -> Vec<PlannedPackage> {
    pkgs.iter()
        .filter(|pkg| {
            vcs_package(pkg)
                || !local.is_some_and(|db| db.satisfies(&pkg.meta.name, &pkg.meta.version))
        })
        .cloned()
        .collect()
}

fn preflight_aur(pkgs: &[PlannedPackage], report: bool) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    if !report && unsafe { libc::geteuid() } == 0 {
        bail!("run ordinary AUR installs as your normal user; only --report requires sudo");
    }
    ensure_bwrap()?;
    crate::archive::check_handoff_policy()?;
    Ok(())
}

pub(crate) fn validate_package_names(packages: &[String]) -> Result<()> {
    if let Some(name) = packages
        .iter()
        .find(|name| name.is_empty() || name.starts_with('-'))
    {
        bail!("invalid package name {name:?}: package names cannot be empty or start with '-'");
    }
    Ok(())
}

/// Turns a plain "not found" into something actionable: a name like
/// `cuttlefish` has no exact match anywhere even though `cuttlefish-base-git`
/// very much exists on the AUR, and the bare resolver error gives no way to
/// discover that. Best-effort — if the AUR search itself fails (offline,
/// AUR down), suggestions from official repos still show, and the error
/// just falls back to the plain "not found" message otherwise.
fn not_found_with_suggestions(
    official: &OfficialRepos,
    aur: &AurClient,
    name: &str,
    required_by: &str,
) -> anyhow::Error {
    let hits = crate::search::find(official, aur, name);
    let mut msg = format!(
        "package '{name}' not found in official repos or the AUR (required by {required_by})"
    );
    if !hits.is_empty() {
        msg.push_str("\n\nDid you mean:\n");
        for hit in hits.iter().take(8) {
            msg.push_str(&format!("  [{}] {}", hit.source, hit.name));
            if let Some(desc) = &hit.description {
                msg.push_str(&format!(" — {desc}"));
            }
            msg.push('\n');
        }
        msg.push_str(&format!("\nRun `archon search {name}` to see all matches."));
    } else {
        msg.push_str(&format!(
            "\n\nNo similarly-named packages found either — try `archon search {name}`."
        ));
    }
    anyhow::anyhow!(msg)
}

fn handle_typo_correction(
    official: &OfficialRepos,
    aur: &AurClient,
    typo: &str,
) -> Result<Option<String>> {
    use archon_resolver::suggest::{PackageRef, suggest};
    use std::io::IsTerminal;

    let mut candidates = Vec::new();

    // 1. Official repos
    for (name, repo) in official.all_package_names() {
        candidates.push(PackageRef {
            name,
            source: "pacman".to_string(),
            repo: Some(repo),
        });
    }

    // 2. AUR (fetch by prefix if long enough)
    let prefix = crate::correction::search_prefix(typo);
    if let Ok(aur_hits) = aur.search_by_name(prefix) {
        for hit in aur_hits {
            candidates.push(PackageRef {
                name: hit.name,
                source: "aur".to_string(),
                repo: None,
            });
        }
    }
    let suggs = suggest(typo, &candidates, 5);
    if suggs.is_empty() {
        return Ok(None);
    }

    let is_tty =
        std::io::stdin().is_terminal() || std::env::var("ARCHON_FORCE_INTERACTIVE").is_ok();
    if !is_tty {
        let names = suggs
            .iter()
            .map(|s| s.pkg.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "package not found, did you mean: {}? re-run interactively or with the exact name",
            names
        );
        std::process::exit(1);
    }

    println!("\nPackage '{}' not found. Did you mean:", typo);
    for (i, s) in suggs.iter().enumerate() {
        let repo_str = match &s.pkg.repo {
            Some(r) => format!("{}/{}", s.pkg.source, r),
            None => s.pkg.source.clone(),
        };
        println!("  {}) {} ({})", i + 1, s.pkg.name, repo_str);
    }

    use std::io::Write;
    let mut attempts = 0;
    while attempts < 3 {
        print!("\nInstall [1]? (Enter to accept, or type a number, or 'n' to cancel): ");
        std::io::stdout().flush().ok();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let input = input.trim();
            if input.eq_ignore_ascii_case("n")
                || input.eq_ignore_ascii_case("q")
                || input.eq_ignore_ascii_case("cancel")
            {
                println!("Cancelled.");
                std::process::exit(130);
            }
            if input.is_empty() {
                return Ok(Some(suggs[0].pkg.name.clone()));
            }
            if let Ok(idx) = input.parse::<usize>()
                && idx > 0
                && idx <= suggs.len()
            {
                return Ok(Some(suggs[idx - 1].pkg.name.clone()));
            }
            println!(
                "Invalid selection. Please enter a number between 1 and {}, or 'n' to cancel.",
                suggs.len()
            );
        } else {
            break;
        }
        attempts += 1;
    }

    println!("Too many invalid attempts. Aborting.");
    std::process::exit(1);
}

/// Delegates entirely to pacman: archon builds the argument list, pacman
/// does its own confirmation, privilege escalation, and transaction. We
/// never touch pacman's databases ourselves. pacman's own output stays
/// visible (unlike the spinners elsewhere) — it's the real system
/// operation, and its progress bars are the user's own pacman.conf
/// styling, `ILoveCandy` or otherwise.
///
/// Only passes pacman the packages that actually need it: without this
/// filter, an already-mostly-installed tree (`okular`'s 424, say) would
/// dump the full list on the command line and print an "up to date --
/// skipping" line for every single one already satisfied — all correct,
/// just not information the plan display didn't already condense away.
fn synchronize_system() -> Result<()> {
    println!("{}", color::dim("sudo pacman -Syu"));
    let status = Command::new("sudo")
        .args(["pacman", "-Syu"])
        .status()
        .context("failed to execute pacman repository refresh and system upgrade")?;
    if !status.success() {
        bail!(
            "repository refresh/system upgrade failed ({status}); installation stopped. Resolve pacman's reported error before retrying. If package downloads still return 404 after refreshing, check mirror synchronization or run `sudo pacman -Syyu`."
        );
    }
    Ok(())
}

fn install_official(pkgs: &[PlannedPackage], local: Option<&archon_repo::LocalDb>) -> Result<()> {
    let names: Vec<&str> = pkgs
        .iter()
        .filter(|p| !local.is_some_and(|db| db.satisfies(&p.meta.name, &p.meta.version)))
        .map(|p| p.meta.name.as_str())
        .collect();
    if names.is_empty() {
        println!(
            "{}",
            color::dim("all resolved official packages are installed and up to date")
        );
        return Ok(());
    }
    println!(
        "{}",
        color::dim(&format!("pacman -S --needed {}", names.join(" ")))
    );
    let status = Command::new("sudo")
        .arg("pacman")
        .arg("-S")
        .arg("--needed")
        .args(&names)
        .status()
        .context("failed to execute pacman (is it installed and in PATH?)")?;
    if !status.success() {
        bail!(
            "pacman exited with {status}; installation stopped before AUR builds. For package download 404s, retry with `archon install <packages> --refresh` (full system upgrade); if they persist, check mirror synchronization or use `sudo pacman -Syyu`."
        );
    }
    Ok(())
}

fn ensure_bwrap() -> Result<()> {
    if !SandboxRunner::bwrap_available() {
        bail!(
            "bubblewrap (bwrap) is not installed — required to build AUR packages safely.\n\
             Install it with: sudo pacman -S bubblewrap"
        );
    }
    Ok(())
}

/// Fetch current recipes without overwriting local edits or diverged commits.
fn clone_if_needed(
    aur: &AurClient,
    pkg: &PlannedPackage,
    build_root: &Path,
    user: Option<&Privileges>,
) -> Result<PathBuf> {
    let base = pkg.meta.build_base();
    if base.is_empty()
        || base == "."
        || base == ".."
        || base.starts_with('-')
        || !base
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"@._+-".contains(&c))
    {
        bail!("invalid AUR package base {base:?}");
    }
    let dest = build_root.join(base);
    let url = aur.clone_url_for_base(base);
    if dest.exists() {
        let _lock = crate::build_lock::BuildLock::acquire(&dest)?;
        if !dest.join(".git").exists() {
            bail!(
                "cached source {} is not a Git checkout; move it aside and retry",
                dest.display()
            );
        }
        let mut status = Command::new("git");
        status
            .arg("-C")
            .arg(&dest)
            .args(["status", "--porcelain", "--untracked-files=no"]);
        let child = spawn_source_command(status, user, &dest)?;
        let mut child = subprocess::ManagedChild::new(child);
        let capture = OutputCapture::start(&mut child);
        let status_code = child.wait().context("cannot check cached AUR source")?;
        let status_output = capture.finish()?;
        if !status_code.success() || !status_output.trim().is_empty() {
            bail!(
                "cached source {} has local changes or is unreadable; commit or move your edits before retrying",
                dest.display()
            );
        }
        let mut fetch = Command::new("git");
        fetch
            .arg("-C")
            .arg(&dest)
            .args(["fetch", "--no-tags", "--progress", "--"])
            .arg(&url)
            .arg("HEAD");
        run_source_command(fetch, user, &dest, &format!("Refreshing {base}"))?;
        let mut merge = Command::new("git");
        merge.arg("-C").arg(&dest).args([
            "-c",
            "core.hooksPath=/dev/null",
            "merge",
            "--ff-only",
            "FETCH_HEAD",
        ]);
        run_source_command(
            merge,
            user,
            &dest,
            &format!("Updating {base} (fast-forward only)"),
        )?;
    } else {
        let mut clone = Command::new("git");
        clone
            .args(["clone", "--progress", "--"])
            .arg(&url)
            .arg(&dest);
        run_source_command(clone, user, build_root, &format!("Cloning {base}"))?;
    }
    Ok(dest)
}

fn spawn_source_command(
    command: Command,
    user: Option<&Privileges>,
    directory: &Path,
) -> Result<std::process::Child> {
    // Cached .git/config is package-writable state: fsmonitor, filters, hooks
    // and transport commands must never execute outside the build boundary.
    let mut args = vec![command.get_program().to_string_lossy().into_owned()];
    args.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    let mut runner = SandboxRunner::new(directory)
        .capture_output(true)
        .with_env("GIT_TERMINAL_PROMPT", "0");
    if let Some(user) = user {
        runner = runner.run_as(user.uid, user.gid);
    }
    runner
        .spawn_command(BuildStage::Fetch, &args)
        .context("cannot launch sandboxed Git")
}

fn run_source_command(
    command: Command,
    user: Option<&Privileges>,
    directory: &Path,
    label: &str,
) -> Result<()> {
    let spinner = Spinner::start(label);
    let child = spawn_source_command(command, user, directory)?;
    let mut child = subprocess::ManagedChild::new(child);
    let capture = OutputCapture::start(&mut child);
    let status = capture.wait_with_progress(&mut child, &spinner, label)?;
    let output = capture.finish()?;
    if !status.success() {
        spinner.fail(label);
        subprocess::print_if_nonempty(&output);
        bail!("{label} failed ({status}); cached sources were not reset or discarded");
    }
    spinner.success(label);
    Ok(())
}

/// Runs both sandbox stages with live output activity and elapsed time.
/// A bounded output tail is retained for diagnosing failures.
pub(crate) fn build_in_sandbox(
    runner: &SandboxRunner,
    pkg_dir: &Path,
    pkg_name: &str,
) -> Result<()> {
    crate::pgp::import_build_keys(pkg_dir);
    for stage in [BuildStage::Fetch, BuildStage::Build] {
        let (action, completed, failure) = match stage {
            BuildStage::Fetch => ("Fetching", "Fetched", "Fetch failed for"),
            BuildStage::Build => ("Building", "Built", "Build failed for"),
        };
        let label = format!("{action} {pkg_name}");
        let spinner = Spinner::build(&label, if stage == BuildStage::Fetch { 0 } else { 1 });
        let child = runner
            .spawn_stage(stage)
            .with_context(|| format!("failed to start {stage:?} stage for {pkg_name}"))?;
        let mut child = subprocess::ManagedChild::new(child);
        let capture = OutputCapture::start(&mut child);
        let status = capture
            .wait_with_progress(&mut child, &spinner, &label)
            .context("failed to wait on sandboxed build")?;
        let output = capture.finish()?;
        if !status.success() {
            spinner.fail(format!("{failure} {pkg_name}"));
            subprocess::print_if_nonempty(&output);
            bail!("sandboxed build failed for {pkg_name} ({stage:?} stage)");
        }
        spinner.success(format!("{completed} {pkg_name}"));
    }
    Ok(())
}

/// Build and install in dependency order so each AUR dependency is
/// available before building its dependents. Pacman owns installation,
/// privilege elevation and its transaction confirmation.
fn build_aur_packages(aur: &AurClient, pkgs: &[PlannedPackage]) -> Result<()> {
    build_aur_packages_with_installer(aur, pkgs, &RootInstaller)
}

fn build_aur_packages_with_installer(
    aur: &AurClient,
    pkgs: &[PlannedPackage],
    installer: &dyn ArchiveInstaller,
) -> Result<()> {
    ensure_bwrap()?;
    installer.preflight()?;
    let build_root = build_dir()?;
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create build dir {}", build_root.display()))?;

    let _build_root_lock = crate::build_lock::BuildLock::acquire(&build_root)?;
    println!();
    for group in aur_build_groups(pkgs) {
        let pkg = group[0];
        let dest = clone_if_needed(aur, pkg, &build_root, None)?;
        let _package_lock = crate::build_lock::BuildLock::acquire(&dest)?;
        let runner = SandboxRunner::new(&dest).capture_output(true);
        build_in_sandbox(&runner, &dest, pkg.meta.build_base())?;
        let files = built_group_files(&dest, &group)?;
        println!(
            "{} Installing {} with pacman",
            color::cyan("→"),
            color::bold(&pkg.meta.name)
        );
        installer.install(&files)?;
    }
    println!("\nInstalled {} AUR package(s).", pkgs.len());
    Ok(())
}

/// Keep a package base in one build and one installation transaction.
fn aur_build_groups(pkgs: &[PlannedPackage]) -> Vec<Vec<&PlannedPackage>> {
    let mut groups: Vec<Vec<&PlannedPackage>> = Vec::new();
    for pkg in pkgs {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group[0].meta.build_base() == pkg.meta.build_base())
        {
            group.push(pkg);
        } else {
            groups.push(vec![pkg]);
        }
    }
    groups
}

fn built_group_files(dest: &Path, group: &[&PlannedPackage]) -> Result<Vec<PackageArchive>> {
    let mut outputs = Vec::new();
    for pkg in group {
        let files = find_package_archives(
            dest,
            &pkg.meta.name,
            Some((&pkg.meta.version, vcs_package(pkg))),
        )?;
        if files.len() != 1 {
            bail!(
                "expected exactly one built package archive for {} at planned version {} in {}, found {}; refusing stale, missing or ambiguous output",
                pkg.meta.name,
                pkg.meta.version,
                dest.display(),
                files.len()
            );
        }
        outputs.extend(files);
    }
    Ok(outputs)
}

/// `--report` requires the eBPF monitor, which requires root. Loading it
/// as root while still spawning `makepkg` as root would fail outright
/// (`makepkg` refuses), so this path only works invoked as `sudo archon
/// install ... --report`: archon loads the monitor as root, then drops
/// back to `$SUDO_UID`/`$SUDO_GID` (the user sudo remembers) for the
/// actual sandboxed build. A direct root login has no such variables to
/// drop to, so it's rejected rather than guessing.
pub(crate) struct Privileges {
    pub uid: u32,
    pub gid: u32,
}

pub(crate) fn require_monitoring_privileges() -> Result<Privileges> {
    if unsafe { libc::geteuid() } != 0 {
        bail!(
            "--report needs root to load the eBPF monitor. Re-run as:\n\
             \n    sudo archon install {} --report\n\
             \nThe build itself still runs as your normal user — archon drops privileges \
             for the sandboxed build automatically.",
            "<packages>"
        );
    }
    let uid: u32 = std::env::var("SUDO_UID")
        .ok()
        .and_then(|s| s.parse().ok())
        .context(
            "running as root but $SUDO_UID is unset — invoke via `sudo`, not a direct root \
             login, so archon knows which user to drop back to for the build",
        )?;
    let gid: u32 = std::env::var("SUDO_GID")
        .ok()
        .and_then(|s| s.parse().ok())
        .context("running as root but $SUDO_GID is unset (see $SUDO_UID note above)")?;
    if uid == 0 || gid == 0 {
        bail!("--report requires a non-root build user and group");
    }
    Ok(Privileges { uid, gid })
}

/// Phase 3+ path: clone, build under eBPF observation, evaluate the
/// policy, print a report, and only offer `pacman -U` if it's clean.
fn build_aur_packages_monitored(aur: &AurClient, pkgs: &[PlannedPackage]) -> Result<()> {
    ensure_bwrap()?;
    let privileges = require_monitoring_privileges()?;
    crate::archive::check_handoff_policy()?;

    let mut monitor = load_monitor()?;

    // Under `sudo`, $HOME is root's home, not the invoking user's — using
    // it here would create a root-owned build dir that the sandbox (which
    // runs as the dropped-to non-root user) couldn't write into. Resolve
    // the real target user's home instead.
    let user_home = home_dir_for_uid(privileges.uid)?;
    let build_root = build_dir_under(&user_home);
    let mut mkdir = Command::new("/usr/bin/mkdir");
    mkdir.args(["-p", "--"]).arg(&build_root);
    as_build_user(&mut mkdir, &privileges)?;
    if !mkdir.status()?.success() {
        bail!("cannot create build directory as the build user; fix its ownership before retrying");
    }
    let _build_root_lock = crate::build_lock::BuildLock::acquire(&build_root)?;
    let store = open_monitor_store()?;

    for group in aur_build_groups(pkgs) {
        let pkg = group[0];
        let dest = clone_if_needed(aur, pkg, &build_root, Some(&privileges))?;
        let _package_lock = crate::build_lock::BuildLock::acquire(&dest)?;
        let report = run_monitored_build(
            &mut monitor,
            &store,
            &dest,
            pkg.meta.build_base(),
            &privileges,
        )?;

        ui::print_report(&report);
        if !report.is_clean() {
            bail!(
                "refusing to install AUR base {}: monitoring reported violations or captured no events",
                pkg.meta.build_base()
            );
        }

        let pkg_files = built_group_files(&dest, &group)?;
        if !ui::confirm(&format!("Install {} now via pacman -U?", pkg.meta.name))? {
            println!("skipped — package left at {}", dest.display());
            continue;
        }
        println!(
            "==> pacman -U {}",
            pkg_files
                .iter()
                .map(|p| p.source().display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        RootInstaller.install(&pkg_files)?;
    }

    Ok(())
}

/// A dedicated cgroup for one build. Membership is inherited through
/// `fork()` by the kernel, so joining it (via
/// [`archon_sandbox::SandboxRunner::join_cgroup`], from inside the
/// sandboxed process's own `pre_exec` — see that method's doc comment for
/// why it must happen there and not from this side) automatically covers
/// every descendant the build spawns. This replaced an earlier
/// PID-propagation design that relied on guessing a kernel tracepoint's
/// byte layout and turned out, on live testing, to silently never track
/// anything past the first process.
struct BuildCgroup {
    path: PathBuf,
}

const CGROUP_ROOT: &str = "/sys/fs/cgroup/archon";

impl BuildCgroup {
    fn create(pkg_name: &str) -> Result<Self> {
        std::fs::create_dir_all(CGROUP_ROOT)
            .context("failed to create /sys/fs/cgroup/archon (cgroups v2 required)")?;
        let path =
            PathBuf::from(CGROUP_ROOT).join(format!("build-{pkg_name}-{}", std::process::id()));
        std::fs::create_dir(&path)
            .with_context(|| format!("failed to create cgroup {}", path.display()))?;
        Ok(Self { path })
    }

    /// The ID `bpf_get_current_cgroup_id()` reports for processes in this
    /// cgroup — cgroups v2 exposes it as the directory's inode.
    fn id(&self) -> Result<u64> {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(&self.path)?.ino())
    }
}

impl Drop for BuildCgroup {
    fn drop(&mut self) {
        // Best-effort: a leftover empty cgroup is harmless clutter, not a
        // security issue, and removal can legitimately race a not-yet-reaped
        // process.
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// Runs both build stages under eBPF observation, evaluating every event
/// against the package's policy and recording it to the store as it
/// arrives (not just at the end, so a crash mid-build still leaves a
/// partial record instead of losing everything).
pub(crate) fn run_monitored_build(
    monitor: &mut Monitor,
    store: &EventStore,
    dest: &Path,
    pkg_name: &str,
    privileges: &Privileges,
) -> Result<BuildReport> {
    // Preparation also crosses the sandbox boundary as the build user.
    // Never touch a package-controlled .gnupg from the privileged parent.
    let key_runner = SandboxRunner::new(dest).run_as(privileges.uid, privileges.gid);
    crate::pgp::import_build_keys_with_runner(dest, &key_runner);

    let allowed_ips = srcinfo::resolve_allowed_ips(dest);
    let policy = Policy::new(dest, allowed_ips);
    let build_id = store
        .start_build(pkg_name, unix_now())
        .context("failed to record build start")?;

    let cgroup = BuildCgroup::create(pkg_name)?;
    monitor
        .set_target_cgroup(cgroup.id()?)
        .context("failed to point the monitor at the build's cgroup")?;

    // The cgroup join happens inside bwrap's own pre_exec (see
    // SandboxRunner::join_cgroup's doc comment) — moving it from the
    // parent after spawn() would race bwrap's internal fork for its
    // --unshare-pid init process, which live testing showed reliably
    // losing that race.
    let runner = SandboxRunner::new(dest)
        .run_as(privileges.uid, privileges.gid)
        .join_cgroup(&cgroup.path)
        .capture_output(true);

    let mut total_events = 0usize;
    let mut flagged = Vec::new();

    for stage in [BuildStage::Fetch, BuildStage::Build] {
        let (action, completed, failure) = match stage {
            BuildStage::Fetch => ("Fetching", "Fetched", "Fetch failed for"),
            BuildStage::Build => ("Building · monitored", "Built", "Build failed for"),
        };
        let label = format!("{action} {pkg_name}");
        let spinner = Spinner::build(&label, if stage == BuildStage::Fetch { 0 } else { 1 });
        let child = runner
            .spawn_stage(stage)
            .with_context(|| format!("failed to start {stage:?} stage for {pkg_name}"))?;
        let mut child = subprocess::ManagedChild::new(child);
        let capture = OutputCapture::start(&mut child);

        let mut last_progress = std::time::Instant::now();
        let status = loop {
            if last_progress.elapsed() >= Duration::from_millis(200) {
                capture.update_progress(&spinner, &label);
                last_progress = std::time::Instant::now();
            }
            if let Some(status) = child.try_wait().context("failed to poll build process")? {
                break status;
            }
            drain_and_record(
                monitor,
                store,
                &policy,
                build_id,
                &mut total_events,
                &mut flagged,
            )?;
            monitor.flush_logs();
            std::thread::sleep(Duration::from_millis(20));
        };
        // Settle before the final drain: a process can exit within
        // milliseconds of its last syscall (observed live — `curl`
        // failing fast under the network-isolated build stage), and while
        // ring buffer submissions are normally visible to userspace
        // immediately, giving the kernel a beat before the last read
        // costs nothing here and removes any doubt.
        std::thread::sleep(Duration::from_millis(100));
        drain_and_record(
            monitor,
            store,
            &policy,
            build_id,
            &mut total_events,
            &mut flagged,
        )?;
        let output = capture.finish()?;

        if !status.success() {
            spinner.fail(format!("{failure} {pkg_name}"));
            subprocess::print_if_nonempty(&output);
            bail!("{stage:?} stage failed for {pkg_name}: makepkg exited with {status}");
        }
        if stage == BuildStage::Build {
            spinner.success(format!(
                "{completed} {pkg_name} · {total_events} events observed"
            ));
        } else {
            spinner.success(format!("{completed} {pkg_name}"));
        }
    }

    Ok(BuildReport {
        package: pkg_name.to_string(),
        total_events,
        flagged,
    })
}

fn drain_and_record(
    monitor: &mut Monitor,
    store: &EventStore,
    policy: &Policy,
    build_id: i64,
    total_events: &mut usize,
    flagged: &mut Vec<FlaggedEvent>,
) -> Result<()> {
    let events = monitor
        .drain_events()
        .context("failed to read monitor events")?
        .into_iter()
        .map(|event| {
            let reason = policy.evaluate(&event);
            (event, reason)
        })
        .collect::<Vec<_>>();
    store
        .record_batch(build_id, &events)
        .context("failed to record monitor event batch")?;
    *total_events += events.len();
    for (event, reason) in events {
        if let Some(reason) = reason {
            flagged.push(FlaggedEvent { event, reason });
        }
    }
    Ok(())
}

pub(crate) fn find_package_files(dest: &Path, pkg_name: &str) -> Result<Vec<PathBuf>> {
    Ok(find_package_archives(dest, pkg_name, None)?
        .into_iter()
        .map(|archive| archive.source().to_path_buf())
        .collect())
}

fn find_package_archives(
    dest: &Path,
    pkg_name: &str,
    expected: Option<(&archon_types::Version, bool)>,
) -> Result<Vec<PackageArchive>> {
    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(dest).with_context(|| format!("failed to read {}", dest.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // The base package's tarball only — not `<name>-debug-*` splits,
        // which pacman would need installed separately and deliberately.
        if name.starts_with(&format!("{pkg_name}-"))
            && name.contains(".pkg.tar")
            && !name.ends_with(".sig")
            && !name.contains("-debug-")
            && entry.file_type()?.is_file()
        {
            let archive = PackageArchive::snapshot(&entry.path())?;
            let identity = Command::new("pacman")
                .args(["-Qp", "--"])
                .arg(archive.path())
                .output()
                .context("failed to query built package identity with pacman -Qp")?;
            if !identity.status.success() {
                bail!(
                    "cannot read built package {}: {}",
                    entry.path().display(),
                    String::from_utf8_lossy(&identity.stderr)
                );
            }
            let metadata =
                String::from_utf8(identity.stdout).context("invalid package identity output")?;
            let mut fields = metadata.split_whitespace();
            if fields.next() == Some(pkg_name) {
                let version = fields
                    .next()
                    .context("package identity is missing its version")?;
                if let Some((expected, vcs)) = expected {
                    let actual = archon_types::Version::parse(version)?;
                    let comparison = actual.vercmp(expected)?;
                    if (!vcs && comparison != std::cmp::Ordering::Equal)
                        || (vcs && comparison == std::cmp::Ordering::Less)
                    {
                        continue;
                    }
                }
                files.push(archive);
            }
        }
    }
    Ok(files)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn build_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("could not determine cache directory (set XDG_CACHE_HOME or HOME)")?;
    Ok(base.join("archon").join("build"))
}

/// Same layout as [`build_dir`], but rooted at an explicitly resolved home
/// directory instead of `$HOME`/`$XDG_CACHE_HOME` — both of which reflect
/// *root's* environment under `sudo`, not the invoking user's.
pub(crate) fn build_dir_under(home: &Path) -> PathBuf {
    home.join(".cache").join("archon").join("build")
}

/// Loads and attaches the eBPF monitor. Shared by the AUR (`install
/// --report`) and local (`build --report`) monitored paths. The object
/// itself is embedded in the binary at compile time (see
/// `archon_monitor::loader::EMBEDDED_EBPF_OBJECT`) — no separate build step
/// or `$ARCHON_EBPF_OBJECT` needed for a normal `cargo install`'d archon.
pub(crate) fn load_monitor() -> Result<Monitor> {
    let bytes = archon_monitor::loader::resolve_ebpf_bytes()
        .context("failed to read $ARCHON_EBPF_OBJECT (unset it to use the built-in monitor)")?;
    let spinner = Spinner::start("Loading eBPF monitor");
    let monitor = Monitor::load_and_attach(&bytes).context("failed to load eBPF monitor");
    match &monitor {
        Ok(_) => spinner.success("eBPF monitor attached"),
        Err(_) => spinner.fail("eBPF monitor failed to load"),
    }
    monitor
}

/// Resolves a uid's home directory via `getent`, independent of any
/// environment variable (which under `sudo` describes root, not the
/// invoking user).
pub(crate) fn home_dir_for_uid(uid: u32) -> Result<PathBuf> {
    let output = Command::new("getent")
        .arg("passwd")
        .arg(uid.to_string())
        .output()
        .context(
            "failed to execute getent (needed to resolve the invoking user's home directory)",
        )?;
    if !output.status.success() {
        bail!("getent passwd {uid} found no matching user — cannot resolve a home directory");
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let home = line
        .trim()
        .split(':')
        .nth(5)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("unexpected `getent passwd {uid}` output: {line:?}"))?;
    Ok(PathBuf::from(home))
}

/// Drop all credentials in the child, before executing Git or filesystem tools.
pub(crate) fn as_build_user(command: &mut Command, user: &Privileges) -> Result<()> {
    use std::os::unix::process::CommandExt;
    if user.uid == 0 || user.gid == 0 {
        bail!("build identity must not be root");
    }
    let (uid, gid) = (user.uid, user.gid);
    command.env("HOME", home_dir_for_uid(uid)?);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(1, &gid) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

/// Root never opens SQLite under a user-writable build/cache tree.
pub(crate) fn open_monitor_store() -> Result<EventStore> {
    use std::os::unix::fs::MetadataExt;
    let staging = crate::handoff::prepare_root()?;
    let path = staging
        .parent()
        .context("missing monitor state parent")?
        .join("monitor.db");
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && (!metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0)
    {
        bail!("unsafe monitor database {}", path.display());
    }
    EventStore::open(&path).context("cannot open administrator-owned monitor database")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str) -> PlannedPackage {
        PlannedPackage {
            meta: archon_types::PackageMeta {
                name: name.into(),
                package_base: Some(name.into()),
                version: archon_types::Version::parse("1-1").unwrap(),
                description: None,
                source: archon_types::PackageSource::Aur,
                depends: vec![],
                make_depends: vec![],
                conflicts: vec![],
                provides: vec![],
            },
            required_by: vec![],
        }
    }

    #[test]
    fn current_aur_packages_are_not_rebuilt_but_vcs_packages_remain_pending() {
        let local = tempfile::tempdir().unwrap();
        for name in ["app", "app-git"] {
            let path = local.path().join(format!("{name}-1-1"));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(
                path.join("desc"),
                format!("%NAME%\n{name}\n\n%VERSION%\n1-1\n"),
            )
            .unwrap();
        }
        let db = archon_repo::LocalDb::load(local.path()).unwrap();
        let pending = pending_aur(
            &[package("app"), package("app-git"), package("missing")],
            Some(&db),
        );
        assert_eq!(
            pending
                .iter()
                .map(|p| p.meta.name.as_str())
                .collect::<Vec<_>>(),
            ["app-git", "missing"]
        );
    }

    #[test]
    fn cached_sources_fast_forward_and_preserve_local_edits_and_divergence() {
        fn git(directory: &Path, args: &[&str]) {
            let result = Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        fn commit(directory: &Path, text: &str) {
            std::fs::write(directory.join("PKGBUILD"), text).unwrap();
            git(directory, &["add", "PKGBUILD"]);
            git(
                directory,
                &[
                    "-c",
                    "user.name=Archon test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-m",
                    "fixture",
                ],
            );
        }
        if !SandboxRunner::bwrap_available() {
            eprintln!("requires bubblewrap for real source-isolation testing");
            return;
        }
        // /var/tmp stays visible read-only inside the sandbox, unlike /tmp.
        let root = tempfile::Builder::new()
            .prefix("archon-git-fixture-")
            .tempdir_in("/var/tmp")
            .unwrap();
        let remote = root.path().join("fixture.git");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "-b", "main"]);
        commit(&remote, "version one");
        let builds = tempfile::tempdir().unwrap();
        let aur = AurClient::new(format!("file://{}", root.path().display())).unwrap();
        let pkg = package("fixture");
        let dest = clone_if_needed(&aur, &pkg, builds.path(), None).unwrap();
        commit(&remote, "version two");
        clone_if_needed(&aur, &pkg, builds.path(), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("PKGBUILD")).unwrap(),
            "version two"
        );
        let marker = root.path().join("host-marker");
        std::fs::write(&marker, "unchanged").unwrap();
        let command = format!("printf compromised > {}", marker.display());
        git(&dest, &["config", "core.fsmonitor", &command]);
        // A package could leave executable Git configuration in its cache.
        // The write attempt must remain inside the read-only host boundary.
        let _ = clone_if_needed(&aur, &pkg, builds.path(), None);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "unchanged");
        git(&dest, &["config", "--unset", "core.fsmonitor"]);
        std::fs::write(dest.join("PKGBUILD"), "my edits").unwrap();
        assert!(clone_if_needed(&aur, &pkg, builds.path(), None).is_err());
        assert_eq!(
            std::fs::read_to_string(dest.join("PKGBUILD")).unwrap(),
            "my edits"
        );
        commit(&dest, "local commit");
        commit(&remote, "version three");
        assert!(clone_if_needed(&aur, &pkg, builds.path(), None).is_err());
        assert_eq!(
            std::fs::read_to_string(dest.join("PKGBUILD")).unwrap(),
            "local commit"
        );
    }

    #[test]
    fn root_build_identities_are_refused_before_spawning_tools() {
        let mut command = Command::new("false");
        assert!(as_build_user(&mut command, &Privileges { uid: 0, gid: 1000 }).is_err());
        assert!(as_build_user(&mut command, &Privileges { uid: 1000, gid: 0 }).is_err());
    }
}
