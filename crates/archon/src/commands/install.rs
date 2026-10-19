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

use anyhow::{bail, Context, Result};
use archon_aur::AurClient;
use archon_distro::Distro;
use archon_monitor::{BuildReport, EventStore, FlaggedEvent, Monitor, Policy};
use archon_repo::OfficialRepos;
use archon_resolver::{PlannedPackage, ResolveError, Resolver};
use archon_sandbox::{BuildStage, SandboxRunner};
use archon_types::PackageProvider;

use super::subprocess::{self, OutputCapture};
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

/// Best-effort: used only to keep the plan display honest about what's
/// already on the system, never for resolution itself, so any failure
/// (missing dir, permissions) just falls back to showing everything —
/// the same as if this lookup didn't exist at all.
fn load_local_db() -> Option<archon_repo::LocalDb> {
    archon_repo::LocalDb::load_cached(Path::new(&local_dir()), &cache_dir().join("local-db.json")).ok()
}

/// Non-Arch path. There's no archon-computed dependency graph here to show
/// a plan for, so unlike the Arch path there's no separate archon-side
/// `[y/N]` prompt either — the native tool's own transaction summary and
/// confirmation *is* the confirmation, exactly as if you'd run it
/// yourself. `--yes` still means something: it maps to the native tool's
/// own assume-yes flag, the same way `--yes` on Arch skips archon's prompt
/// without touching pacman's.
fn run_foreign_distro(distro: &Distro, packages: &[String], dry_run: bool, yes: bool, report: bool) -> Result<()> {
    if report {
        println!(
            "{}",
            color::yellow(
                "note: --report (sandboxed, eBPF-monitored builds) needs a PKGBUILD-equivalent \
                 build recipe, which only Arch/AUR has today — installing normally instead."
            )
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
pub(crate) fn run_native_tool(backend: &dyn archon_distro::Backend, argv: &[String], dry_run: bool) -> Result<()> {
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
    let status = cmd
        .status()
        .with_context(|| format!("failed to execute {} (is it installed and in PATH?)", backend.name()))?;
    if !status.success() {
        bail!("{} exited with {status}", backend.name());
    }
    Ok(())
}

pub fn run(packages: &[String], dry_run: bool, yes: bool, report: bool) -> Result<()> {
    run_with_history(packages, dry_run, yes, report, &std::collections::HashMap::new())
}

fn run_with_history(
    packages: &[String],
    dry_run: bool,
    yes: bool,
    report: bool,
    correction_history: &std::collections::HashMap<usize, Vec<String>>,
) -> Result<()> {
    let distro = Distro::detect();
    if !distro.is_arch() {
        return run_foreign_distro(&distro, packages, dry_run, yes, report);
    }

    let spinner = Spinner::start(format!("Resolving {}", packages.join(", ")));
    let sync_dir = sync_dir();
    let repo_cache = cache_dir().join("sync-db.json");
    let official = match OfficialRepos::load_cached(Path::new(&sync_dir), archon_repo::DEFAULT_REPO_ORDER, &repo_cache)
        .with_context(|| {
            format!(
                "failed to load official repo databases from {sync_dir} \
                 (run `sudo pacman -Sy` if you've never synced)"
            )
        }) {
        Ok(v) => v,
        Err(e) => {
            spinner.fail("resolution failed");
            return Err(e);
        }
    };
    let aur = AurClient::new(aur_url())?;
    let local_db = load_local_db();
    let installed_names: std::collections::HashSet<String> = local_db
        .as_ref()
        .map(|db| db.names().map(str::to_string).collect())
        .unwrap_or_default();

    let providers: Vec<&dyn PackageProvider> = vec![&official, &aur];
    let resolve_result = Resolver::new(providers).prefer_installed(&installed_names).resolve(packages);
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
                    correction_history.get(&pos).map(Vec::as_slice).unwrap_or_default(), &name
                ).map_err(anyhow::Error::msg)?;

                if let Some(corrected) = handle_typo_correction(&official, &aur, &name)? {
                    current_chain.push(corrected.clone());
                    
                    let mut new_history = correction_history.clone();
                    new_history.insert(pos, current_chain);
                    
                    let mut new_packages = packages.to_vec();
                    if pos < new_packages.len() {
                        new_packages[pos] = corrected;
                    }
                    return run_with_history(&new_packages, dry_run, yes, report, &new_history);
                }
            }
            return Err(not_found_with_suggestions(&official, &aur, &name, &required_by));
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
        return Ok(());
    }

    if report {
        require_monitoring_privileges()?;
    }

    if !report && !plan.aur.is_empty() {
        println!("AUR builds use bubblewrap without eBPF monitoring (enable with --report).");
    }

    if !yes && !ui::confirm("\nProceed with installation?")? {
        println!("aborted — nothing was changed.");
        return Ok(());
    }

    if !plan.official.is_empty() {
        install_official(&plan.official, local_db.as_ref())?;
    }

    if !plan.aur.is_empty() {
        if report {
            build_aur_packages_monitored(&aur, &plan.aur)?;
        } else {
            build_aur_packages(&aur, &plan.aur)?;
        }
    }

    Ok(())
}

/// Turns a plain "not found" into something actionable: a name like
/// `cuttlefish` has no exact match anywhere even though `cuttlefish-base-git`
/// very much exists on the AUR, and the bare resolver error gives no way to
/// discover that. Best-effort — if the AUR search itself fails (offline,
/// AUR down), suggestions from official repos still show, and the error
/// just falls back to the plain "not found" message otherwise.
fn not_found_with_suggestions(official: &OfficialRepos, aur: &AurClient, name: &str, required_by: &str) -> anyhow::Error {
    let hits = crate::search::find(official, aur, name);
    let mut msg = format!("package '{name}' not found in official repos or the AUR (required by {required_by})");
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
        msg.push_str(&format!("\n\nNo similarly-named packages found either — try `archon search {name}`."));
    }
    anyhow::anyhow!(msg)
}

fn handle_typo_correction(official: &OfficialRepos, aur: &AurClient, typo: &str) -> Result<Option<String>> {
    use archon_resolver::suggest::{suggest, PackageRef};
    use std::io::IsTerminal;
    
    let mut candidates = Vec::new();
    
    // 1. Official repos
    for (name, repo) in official.all_package_names() {
        candidates.push(PackageRef { name, source: "pacman".to_string(), repo: Some(repo) });
    }
    
    // 2. AUR (fetch by prefix if long enough)
    let prefix = crate::correction::search_prefix(typo);
    if let Ok(aur_hits) = aur.search_by_name(prefix) {
        for hit in aur_hits {
            candidates.push(PackageRef { name: hit.name, source: "aur".to_string(), repo: None });
        }
    }
    let suggs = suggest(typo, &candidates, 5);
    if suggs.is_empty() {
        return Ok(None);
    }
    
    let is_tty = std::io::stdin().is_terminal() || std::env::var("ARCHON_FORCE_INTERACTIVE").is_ok();
    if !is_tty {
        let names = suggs.iter().map(|s| s.pkg.name.as_str()).collect::<Vec<_>>().join(", ");
        eprintln!("package not found, did you mean: {}? re-run interactively or with the exact name", names);
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
            if input.eq_ignore_ascii_case("n") || input.eq_ignore_ascii_case("q") || input.eq_ignore_ascii_case("cancel") {
                println!("Cancelled.");
                std::process::exit(130);
            }
            if input.is_empty() {
                return Ok(Some(suggs[0].pkg.name.clone()));
            }
            if let Ok(idx) = input.parse::<usize>() {
                if idx > 0 && idx <= suggs.len() {
                    return Ok(Some(suggs[idx - 1].pkg.name.clone()));
                }
            }
            println!("Invalid selection. Please enter a number between 1 and {}, or 'n' to cancel.", suggs.len());
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
fn install_official(pkgs: &[PlannedPackage], local: Option<&archon_repo::LocalDb>) -> Result<()> {
    let names: Vec<&str> = pkgs
        .iter()
        .filter(|p| !local.is_some_and(|db| db.satisfies(&p.meta.name, &p.meta.version)))
        .map(|p| p.meta.name.as_str())
        .collect();
    if names.is_empty() {
        println!("{}", color::dim("all official packages already up to date"));
        return Ok(());
    }
    println!("{}", color::dim(&format!("pacman -S --needed {}", names.join(" "))));
    let status = Command::new("sudo")
        .arg("pacman")
        .arg("-S")
        .arg("--needed")
        .args(&names)
        .status()
        .context("failed to execute pacman (is it installed and in PATH?)")?;
    if !status.success() {
        bail!("pacman exited with {status}");
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

/// Clones an AUR package's git repo if not already present. Existing
/// clones are left untouched rather than force-reset, since this is
/// user-visible working state, not disposable scratch space.
fn clone_if_needed(aur: &AurClient, pkg: &PlannedPackage, build_root: &Path) -> Result<PathBuf> {
    let base = pkg.meta.build_base();
    if base.is_empty() || base == "." || base == ".." || base.starts_with('-')
        || !base.bytes().all(|c| c.is_ascii_alphanumeric() || b"@._+-".contains(&c))
    {
        bail!("invalid AUR package base {base:?}");
    }
    let dest = build_root.join(base);
    if dest.exists() {
        println!("{} {} {}", color::yellow("↺"), pkg.meta.name, color::dim("already cloned"));
        return Ok(dest);
    }
    let url = aur.clone_url_for(&pkg.meta.name).with_context(|| {
        format!(
            "no clone URL cached for {} (internal error: resolver should have queried it)",
            pkg.meta.name
        )
    })?;
    let spinner = Spinner::start(format!("Cloning {}", pkg.meta.name));
    let child = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(&url)
        .arg(&dest)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to execute git (is it installed and in PATH?)")?;
    let (status, output) = subprocess::wait_capturing(child)?;
    if !status.success() {
        spinner.fail(format!("clone failed: {}", pkg.meta.name));
        subprocess::print_if_nonempty(&output);
        bail!("git clone failed for {}", pkg.meta.name);
    }
    spinner.success(format!("cloned {}", pkg.meta.name));
    Ok(dest)
}

/// Runs both sandbox stages for one package with a spinner standing in
/// for `makepkg`'s normally very chatty output — captured in the
/// background (see [`subprocess::OutputCapture`]) and only printed if a
/// stage actually fails.
pub(crate) fn build_in_sandbox(runner: &SandboxRunner, pkg_dir: &Path, pkg_name: &str) -> Result<()> {
    crate::pgp::import_build_keys(pkg_dir);
    for stage in [BuildStage::Fetch, BuildStage::Build] {
        let label = match stage {
            BuildStage::Fetch => format!("Fetching {pkg_name}"),
            BuildStage::Build => format!("Building {pkg_name}"),
        };
        let spinner = Spinner::start(label);
        let mut child = runner
            .spawn_stage(stage)
            .with_context(|| format!("failed to start {stage:?} stage for {pkg_name}"))?;
        let capture = OutputCapture::start(&mut child);
        let status = child.wait().context("failed to wait on sandboxed build")?;
        let output = capture.finish();
        if !status.success() {
            spinner.fail(format!("{pkg_name} failed ({stage:?})"));
            subprocess::print_if_nonempty(&output);
            bail!("sandboxed build failed for {pkg_name} ({stage:?} stage)");
        }
        spinner.success(format!("{pkg_name} ({stage:?})"));
    }
    println!("{} {} built", color::bold_green("✓"), pkg_name);
    Ok(())
}

/// Build and install in dependency order so each AUR dependency is
/// available before building its dependents. Pacman owns installation,
/// privilege elevation and its transaction confirmation.
fn build_aur_packages(aur: &AurClient, pkgs: &[PlannedPackage]) -> Result<()> {
    ensure_bwrap()?;
    let build_root = build_dir()?;
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create build dir {}", build_root.display()))?;

    println!();
    for group in aur_build_groups(pkgs) {
        let pkg = group[0];
        let dest = clone_if_needed(aur, pkg, &build_root)?;
        let runner = SandboxRunner::new(&dest).capture_output(true);
        build_in_sandbox(&runner, &dest, pkg.meta.build_base())?;
        let files = built_group_files(&dest, &group)?;
        println!("==> Installing {} via pacman -U", pkg.meta.name);
        let status = Command::new("sudo")
            .args(["pacman", "-U", "--needed", "--"])
            .args(&files)
            .status()
            .context("failed to execute sudo pacman -U")?;
        if !status.success() {
            bail!("pacman -U exited with {status} for {}", pkg.meta.name);
        }
    }
    println!("\nInstalled {} AUR package(s).", pkgs.len());
    Ok(())
}

/// Keep a package base in one build and one installation transaction.
fn aur_build_groups(pkgs: &[PlannedPackage]) -> Vec<Vec<&PlannedPackage>> {
    let mut groups: Vec<Vec<&PlannedPackage>> = Vec::new();
    for pkg in pkgs {
        if let Some(group) = groups.iter_mut().find(|group| group[0].meta.build_base() == pkg.meta.build_base()) {
            group.push(pkg);
        } else {
            groups.push(vec![pkg]);
        }
    }
    groups
}

fn built_group_files(dest: &Path, group: &[&PlannedPackage]) -> Result<Vec<PathBuf>> {
    let mut outputs = Vec::new();
    for pkg in group {
        let files = find_package_files(dest, &pkg.meta.name)?;
        if files.len() != 1 {
            bail!("expected exactly one built package archive for {} in {}, found {}; refusing installation of missing or ambiguous output",
                pkg.meta.name, dest.display(), files.len());
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
    Ok(Privileges { uid, gid })
}

/// Phase 3+ path: clone, build under eBPF observation, evaluate the
/// policy, print a report, and only offer `pacman -U` if it's clean.
fn build_aur_packages_monitored(aur: &AurClient, pkgs: &[PlannedPackage]) -> Result<()> {
    ensure_bwrap()?;
    let privileges = require_monitoring_privileges()?;

    let mut monitor = load_monitor()?;

    // Under `sudo`, $HOME is root's home, not the invoking user's — using
    // it here would create a root-owned build dir that the sandbox (which
    // runs as the dropped-to non-root user) couldn't write into. Resolve
    // the real target user's home instead.
    let user_home = home_dir_for_uid(privileges.uid)?;
    let build_root = build_dir_under(&user_home);
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create build dir {}", build_root.display()))?;
    // Chown the whole archon cache root, not just build/ — the resolver's
    // on-disk cache (archon_repo::cache) lives directly under here too,
    // and a root-owned parent silently blocks every future non-sudo run
    // from ever writing it (cache-write failures are swallowed by design,
    // so on a real system this went unnoticed until measured: caching
    // appeared to do nothing at all, because it never actually happened).
    if let Some(cache_root) = build_root.parent() {
        ensure_owned(cache_root, privileges.uid, privileges.gid)
            .with_context(|| format!("failed to chown {} to the target user", cache_root.display()))?;
    }
    ensure_owned(&build_root, privileges.uid, privileges.gid)
        .with_context(|| format!("failed to chown {} to the target user", build_root.display()))?;
    let store = EventStore::open(&build_root.join("monitor.db")).context("failed to open event store")?;

    for group in aur_build_groups(pkgs) {
        let pkg = group[0];
        let dest = clone_if_needed(aur, pkg, &build_root)?;
        // `git clone` above ran as root; the sandboxed build that's about
        // to run as the dropped-to user needs to write into this tree.
        ensure_owned_recursive(&dest, privileges.uid, privileges.gid)
            .with_context(|| format!("failed to chown {} to the target user", dest.display()))?;
        let report = run_monitored_build(&mut monitor, &store, &dest, pkg.meta.build_base(), &privileges)?;

        ui::print_report(&report);
        if !report.is_clean() {
            println!("{} not installed — see flags above", color::red(&pkg.meta.name));
            continue;
        }

        let pkg_files = built_group_files(&dest, &group)?;
        if !ui::confirm(&format!("Install {} now via pacman -U?", pkg.meta.name))? {
            println!("skipped — package left at {}", dest.display());
            continue;
        }
        println!("==> pacman -U {}", pkg_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" "));
        let status = Command::new("pacman")
            .arg("-U")
            .args(&pkg_files)
            .status()
            .context("failed to execute pacman -U")?;
        if !status.success() {
            bail!("pacman -U exited with {status} for {}", pkg.meta.name);
        }
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
        let path = PathBuf::from(CGROUP_ROOT).join(format!("build-{pkg_name}-{}", std::process::id()));
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
    // Runs as root (this function doesn't itself drop privileges — only
    // the sandboxed bwrap process does), so any `.gnupg` it creates here
    // must be handed to the target user below, same as the rest of `dest`.
    crate::pgp::import_build_keys(dest);
    let gnupg_home = dest.join(".gnupg");
    if gnupg_home.exists() {
        ensure_owned_recursive(&gnupg_home, privileges.uid, privileges.gid)
            .with_context(|| format!("failed to chown {} to the target user", gnupg_home.display()))?;
    }

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
        let label = match stage {
            BuildStage::Fetch => format!("Fetching {pkg_name}"),
            BuildStage::Build => format!("Building {pkg_name} (watched)"),
        };
        let spinner = Spinner::start(label);
        let mut child = runner
            .spawn_stage(stage)
            .with_context(|| format!("failed to start {stage:?} stage for {pkg_name}"))?;
        let capture = OutputCapture::start(&mut child);

        let status = loop {
            if let Some(status) = child.try_wait().context("failed to poll build process")? {
                break status;
            }
            drain_and_record(monitor, store, &policy, build_id, &mut total_events, &mut flagged)?;
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
        drain_and_record(monitor, store, &policy, build_id, &mut total_events, &mut flagged)?;
        let output = capture.finish();

        if !status.success() {
            spinner.fail(format!("{pkg_name} failed ({stage:?})"));
            subprocess::print_if_nonempty(&output);
            bail!("{stage:?} stage failed for {pkg_name}: makepkg exited with {status}");
        }
        spinner.success(format!("{pkg_name} ({stage:?}) — {total_events} events so far"));
    }

    Ok(BuildReport { package: pkg_name.to_string(), total_events, flagged })
}

fn drain_and_record(
    monitor: &mut Monitor,
    store: &EventStore,
    policy: &Policy,
    build_id: i64,
    total_events: &mut usize,
    flagged: &mut Vec<FlaggedEvent>,
) -> Result<()> {
    for event in monitor.drain_events().context("failed to read monitor events")? {
        *total_events += 1;
        let reason = policy.evaluate(&event);
        store
            .record_event(build_id, &event, reason.as_ref())
            .context("failed to record event")?;
        if let Some(reason) = reason {
            flagged.push(FlaggedEvent { event, reason });
        }
    }
    Ok(())
}

pub(crate) fn find_package_files(dest: &Path, pkg_name: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dest).with_context(|| format!("failed to read {}", dest.display()))? {
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
            let identity = Command::new("pacman")
                .args(["-Qp", "--"])
                .arg(entry.path())
                .output()
                .context("failed to query built package identity with pacman -Qp")?;
            if !identity.status.success() {
                bail!("cannot read built package {}: {}", entry.path().display(), String::from_utf8_lossy(&identity.stderr));
            }
            let metadata = String::from_utf8(identity.stdout).context("invalid package identity output")?;
            if metadata.split_whitespace().next() == Some(pkg_name) {
                files.push(entry.path());
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
        .context("failed to execute getent (needed to resolve the invoking user's home directory)")?;
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

/// `create_dir_all` under a root process creates root-owned directories;
/// the sandboxed build then runs as the dropped-to non-root user and needs
/// to write into `path`, so hand ownership back. Non-recursive: `path`'s
/// parents being root-owned is harmless (default 0755 is traversable by
/// anyone), only `path` itself needs to be writable by the target user.
pub(crate) fn ensure_owned(path: &Path, uid: u32, gid: u32) -> Result<()> {
    ensure_owned_impl(path, uid, gid, false)
}

/// As [`ensure_owned`], but recursive — needed after `git clone` (still
/// run as root even on the monitored path) leaves the whole cloned tree
/// root-owned, which the privilege-dropped sandbox then can't write into.
pub(crate) fn ensure_owned_recursive(path: &Path, uid: u32, gid: u32) -> Result<()> {
    ensure_owned_impl(path, uid, gid, true)
}

fn ensure_owned_impl(path: &Path, uid: u32, gid: u32, recursive: bool) -> Result<()> {
    let mut cmd = Command::new("chown");
    if recursive {
        cmd.arg("-R");
    }
    let status = cmd
        .arg(format!("{uid}:{gid}"))
        .arg(path)
        .status()
        .context("failed to execute chown")?;
    if !status.success() {
        bail!("chown exited with {status}");
    }
    Ok(())
}
