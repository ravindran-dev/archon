//! `archon install` / `archon plan`.
//!
//! Flow:
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
//! Without `--report`, the AUR path stops after a successful sandboxed
//! build (Phase 2 behavior) — archon tells you where the package tarball
//! landed but does not install it.
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
use archon_monitor::{BuildReport, EventStore, FlaggedEvent, Monitor, Policy};
use archon_repo::OfficialRepos;
use archon_resolver::{PlannedPackage, Resolver};
use archon_sandbox::{BuildStage, SandboxRunner};
use archon_types::PackageProvider;

use crate::{srcinfo, ui};

const SYNC_DIR: &str = "/var/lib/pacman/sync";

pub fn run(packages: &[String], dry_run: bool, yes: bool, report: bool) -> Result<()> {
    println!("Resolving dependencies for: {}", packages.join(", "));

    let official = OfficialRepos::load(Path::new(SYNC_DIR), archon_repo::DEFAULT_REPO_ORDER)
        .with_context(|| {
            format!(
                "failed to load official repo databases from {SYNC_DIR} \
                 (run `sudo pacman -Sy` if you've never synced)"
            )
        })?;
    let aur = AurClient::new(archon_aur::DEFAULT_AUR_URL)?;

    let providers: Vec<&dyn PackageProvider> = vec![&official, &aur];
    let plan = Resolver::new(providers)
        .resolve(packages)
        .context("dependency resolution failed")?;

    if plan.is_empty() {
        println!("nothing to do — all requested packages already resolved to nothing new.");
        return Ok(());
    }

    println!();
    ui::print_plan(&plan);

    if dry_run {
        return Ok(());
    }

    if report {
        require_monitoring_privileges()?;
    }

    if !yes && !ui::confirm("\nProceed with installation?")? {
        println!("aborted — nothing was changed.");
        return Ok(());
    }

    if !plan.official.is_empty() {
        install_official(&plan.official)?;
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

/// Delegates entirely to pacman: archon builds the argument list, pacman
/// does its own confirmation, privilege escalation, and transaction. We
/// never touch pacman's databases ourselves.
fn install_official(pkgs: &[PlannedPackage]) -> Result<()> {
    let names: Vec<&str> = pkgs.iter().map(|p| p.meta.name.as_str()).collect();
    println!("\n==> sudo pacman -S --needed {}", names.join(" "));
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
    let dest = build_root.join(&pkg.meta.name);
    if dest.exists() {
        println!(
            "==> {} already cloned at {} — reusing (remove it to re-clone)",
            pkg.meta.name,
            dest.display()
        );
        return Ok(dest);
    }
    let url = aur.clone_url_for(&pkg.meta.name).with_context(|| {
        format!(
            "no clone URL cached for {} (internal error: resolver should have queried it)",
            pkg.meta.name
        )
    })?;
    println!("==> cloning {} from {url}", pkg.meta.name);
    let status = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(&url)
        .arg(&dest)
        .status()
        .context("failed to execute git (is it installed and in PATH?)")?;
    if !status.success() {
        bail!("git clone failed for {}", pkg.meta.name);
    }
    Ok(dest)
}

/// Phase 2 path: clone + sandboxed build, no monitoring. Stops after a
/// successful build; installing is only offered on the `--report` path.
fn build_aur_packages(aur: &AurClient, pkgs: &[PlannedPackage]) -> Result<()> {
    ensure_bwrap()?;
    let build_root = build_dir()?;
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create build dir {}", build_root.display()))?;

    println!();
    let mut built = Vec::new();
    for pkg in pkgs {
        let dest = clone_if_needed(aur, pkg, &build_root)?;
        println!("==> building {} in sandbox (fetch stage: network allowed)", pkg.meta.name);
        SandboxRunner::new(&dest)
            .build()
            .with_context(|| format!("sandboxed build failed for {}", pkg.meta.name))?;
        println!("==> {} built successfully", pkg.meta.name);
        built.push(dest);
    }

    println!("\nBuilt {} AUR package(s):", built.len());
    for dest in &built {
        println!("  {}", dest.display());
    }
    println!(
        "\nNot installed. Re-run with `--report` to have archon monitor the build with \
         eBPF and offer to install once it confirms nothing was flagged."
    );
    Ok(())
}

/// `--report` requires the eBPF monitor, which requires root. Loading it
/// as root while still spawning `makepkg` as root would fail outright
/// (`makepkg` refuses), so this path only works invoked as `sudo archon
/// install ... --report`: archon loads the monitor as root, then drops
/// back to `$SUDO_UID`/`$SUDO_GID` (the user sudo remembers) for the
/// actual sandboxed build. A direct root login has no such variables to
/// drop to, so it's rejected rather than guessing.
struct Privileges {
    uid: u32,
    gid: u32,
}

fn require_monitoring_privileges() -> Result<Privileges> {
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

    let obj_path = archon_monitor::loader::default_object_path().context(
        "compiled eBPF object not found. Build it first:\n\
         \n    cd crates/archon-monitor-ebpf && cargo +nightly build --release\n\
         \nor set $ARCHON_EBPF_OBJECT to point at a prebuilt archon-monitor-ebpf object",
    )?;
    println!("\n==> loading eBPF monitor from {}", obj_path.display());
    let mut monitor = Monitor::load_and_attach(&obj_path).context("failed to load eBPF monitor")?;

    // Under `sudo`, $HOME is root's home, not the invoking user's — using
    // it here would create a root-owned build dir that the sandbox (which
    // runs as the dropped-to non-root user) couldn't write into. Resolve
    // the real target user's home instead.
    let user_home = home_dir_for_uid(privileges.uid)?;
    let build_root = build_dir_under(&user_home);
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("failed to create build dir {}", build_root.display()))?;
    ensure_owned(&build_root, privileges.uid, privileges.gid)
        .with_context(|| format!("failed to chown {} to the target user", build_root.display()))?;
    let store = EventStore::open(&build_root.join("monitor.db")).context("failed to open event store")?;

    for pkg in pkgs {
        let dest = clone_if_needed(aur, pkg, &build_root)?;
        // `git clone` above ran as root; the sandboxed build that's about
        // to run as the dropped-to user needs to write into this tree.
        ensure_owned_recursive(&dest, privileges.uid, privileges.gid)
            .with_context(|| format!("failed to chown {} to the target user", dest.display()))?;
        let report = run_monitored_build(&mut monitor, &store, &dest, &pkg.meta.name, &privileges)?;

        println!("\n{report}");
        if !report.is_clean() {
            println!(
                "==> {} was NOT installed — the build was flagged above. \
                 Inspect the report before proceeding manually.",
                pkg.meta.name
            );
            continue;
        }

        let pkg_files = find_package_files(&dest, &pkg.meta.name)?;
        if pkg_files.is_empty() {
            println!("==> {} built clean but no package file was found — skipping install.", pkg.meta.name);
            continue;
        }
        if !ui::confirm(&format!(
            "==> {} passed the build report — install now via pacman -U?",
            pkg.meta.name
        ))? {
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
/// `fork()` by the kernel, so moving the sandboxed process's PID in here
/// (before it execs anything) automatically covers every descendant it
/// spawns — the reason this replaced an earlier PID-propagation design
/// that relied on guessing a kernel tracepoint's byte layout and turned
/// out, on live testing, to silently never track anything past the first
/// process.
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

    /// The ID `bpf_get_current_cgroup_id()` reports for processes moved
    /// into this cgroup — cgroups v2 exposes it as the directory's inode.
    fn id(&self) -> Result<u64> {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(&self.path)?.ino())
    }

    fn add(&self, pid: u32) -> Result<()> {
        std::fs::write(self.path.join("cgroup.procs"), pid.to_string())
            .with_context(|| format!("failed to move pid {pid} into cgroup {}", self.path.display()))
    }
}

/// TEMPORARY diagnostic: prints everything needed to tell whether the
/// cgroup move actually took effect and whether `/sys/fs/cgroup` is truly
/// the unified cgroup v2 hierarchy (a hybrid v1/v2 mount is a common way
/// for a plain `mkdir` under `/sys/fs/cgroup` to not behave like a real
/// cgroup v2 node at all). Remove once cgroup scoping is confirmed working.
fn debug_print_cgroup_state(cgroup: &BuildCgroup, pid: u32) {
    let fs_type = Command::new("stat")
        .arg("-f")
        .arg("-c")
        .arg("%T")
        .arg("/sys/fs/cgroup")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "<stat failed>".into());
    let proc_cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_else(|e| format!("<read failed: {e}>"));
    println!(
        "    [debug] /sys/fs/cgroup fstype={fs_type} archon_cgroup_path={} archon_cgroup_id={:?} /proc/{pid}/cgroup={}",
        cgroup.path.display(),
        cgroup.id(),
        proc_cgroup.trim()
    );
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
fn run_monitored_build(
    monitor: &mut Monitor,
    store: &EventStore,
    dest: &Path,
    pkg_name: &str,
    privileges: &Privileges,
) -> Result<BuildReport> {
    let allowed_ips = srcinfo::resolve_allowed_ips(dest);
    let policy = Policy::new(dest, allowed_ips);
    let build_id = store
        .start_build(pkg_name, unix_now())
        .context("failed to record build start")?;

    let runner = SandboxRunner::new(dest).run_as(privileges.uid, privileges.gid);
    let mut total_events = 0usize;
    let mut flagged = Vec::new();

    let cgroup = BuildCgroup::create(pkg_name)?;
    monitor
        .set_target_cgroup(cgroup.id()?)
        .context("failed to point the monitor at the build's cgroup")?;

    for stage in [BuildStage::Fetch, BuildStage::Build] {
        println!("==> {pkg_name}: running {stage:?} stage under eBPF observation");
        let mut child = runner
            .spawn_stage(stage)
            .with_context(|| format!("failed to start {stage:?} stage for {pkg_name}"))?;
        // Moved in immediately after spawn, before the process has done
        // anything beyond exec setup — every subsequent syscall, and every
        // descendant it forks, is covered from here on.
        cgroup
            .add(child.id())
            .context("failed to move the build process into its cgroup")?;
        debug_print_cgroup_state(&cgroup, child.id());

        let status = loop {
            if let Some(status) = child.try_wait().context("failed to poll build process")? {
                break status;
            }
            drain_and_record(monitor, store, &policy, build_id, &mut total_events, &mut flagged)?;
            monitor.flush_logs();
            std::thread::sleep(Duration::from_millis(50));
        };
        // Final drain: events written in the process's last moments can
        // still be sitting in the ring buffer after wait() returns.
        drain_and_record(monitor, store, &policy, build_id, &mut total_events, &mut flagged)?;

        if !status.success() {
            bail!("{stage:?} stage failed for {pkg_name}: makepkg exited with {status}");
        }
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

fn find_package_files(dest: &Path, pkg_name: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dest).with_context(|| format!("failed to read {}", dest.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // The base package's tarball only — not `<name>-debug-*` splits,
        // which pacman would need installed separately and deliberately.
        if name.starts_with(&format!("{pkg_name}-")) && name.contains(".pkg.tar") && !name.contains("-debug-") {
            files.push(entry.path());
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
fn build_dir_under(home: &Path) -> PathBuf {
    home.join(".cache").join("archon").join("build")
}

/// Resolves a uid's home directory via `getent`, independent of any
/// environment variable (which under `sudo` describes root, not the
/// invoking user).
fn home_dir_for_uid(uid: u32) -> Result<PathBuf> {
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
fn ensure_owned(path: &Path, uid: u32, gid: u32) -> Result<()> {
    ensure_owned_impl(path, uid, gid, false)
}

/// As [`ensure_owned`], but recursive — needed after `git clone` (still
/// run as root even on the monitored path) leaves the whole cloned tree
/// root-owned, which the privilege-dropped sandbox then can't write into.
fn ensure_owned_recursive(path: &Path, uid: u32, gid: u32) -> Result<()> {
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
