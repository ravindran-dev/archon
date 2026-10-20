//! CLI integration tests: exercise the compiled `archon` binary as a black
//! box. Every test here runs against a fixture sync database (built
//! in-memory, see `common::fixture_sync_dir`) via `$ARCHON_SYNC_DIR` —
//! none of them touch the real system's pacman databases, the real AUR,
//! or need root. The few things that genuinely can't be tested this way
//! (the eBPF monitor, real sandboxed builds) are covered by the unit
//! tests in `archon-sandbox`/`archon-monitor` and by manual live testing —
//! see the README's Status section.

mod common;

use common::{
    FixturePkg, UNREACHABLE_AUR_URL, empty_aur_server, empty_cache_dir, empty_local_dir,
    fixture_local_dir, fixture_os_release, fixture_sync_dir, run_archon,
};

#[test]
fn help_lists_all_subcommands() {
    let out = run_archon(&["--help"], &[]);
    assert!(out.status.success());
    assert!(out.stdout.contains("install"));
    assert!(out.stdout.contains("plan"));
    assert!(out.stdout.contains("remove"));
    assert!(out.stdout.contains("build"));
}

#[test]
fn remove_help_lists_flags() {
    let out = run_archon(&["remove", "--help"], &[]);
    assert!(out.status.success());
    assert!(out.stdout.contains("--dry-run"));
    assert!(out.stdout.contains("--yes"));
}

#[test]
fn remove_rejects_package_that_is_not_installed() {
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["remove", "not-installed-xyz", "--dry-run"],
        &[
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(!out.status.success());
    assert!(out.stderr.contains("not installed"), "got: {}", out.stderr);
}

#[test]
fn remove_dry_run_shows_plan_and_stops_before_pacman() {
    let (_local, local_dir) = fixture_local_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["remove", "leaf", "--dry-run"],
        &[
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Remove plan"));
    assert!(out.stdout.contains("leaf"));
    assert!(out.stdout.contains("1.0-1"));
    assert!(
        !out.stdout.contains("pacman -Rs leaf"),
        "dry-run must never reach the actual pacman invocation: {}",
        out.stdout
    );
}

#[test]
fn remove_all_or_nothing_when_one_package_is_missing() {
    let (_local, local_dir) = fixture_local_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["remove", "leaf", "also-not-installed", "--dry-run"],
        &[
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(!out.status.success());
    assert!(out.stderr.contains("also-not-installed"));
    assert!(
        !out.stdout.contains("Remove plan"),
        "must not show a partial plan when part of the request is invalid: {}",
        out.stdout
    );
}

#[test]
fn install_help_lists_report_flag() {
    let out = run_archon(&["install", "--help"], &[]);
    assert!(out.status.success());
    assert!(out.stdout.contains("--report"));
    assert!(out.stdout.contains("--dry-run"));
    assert!(out.stdout.contains("--yes"));
}

#[test]
fn dry_run_resolves_and_stops_before_any_mutation() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "leaf", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL),
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Install plan"));
    assert!(out.stdout.contains("leaf"));
    assert!(out.stdout.contains("1 official"));
    assert!(
        !out.stdout.contains("pacman -S"),
        "dry-run must never reach the pacman invocation: {}",
        out.stdout
    );
}

#[test]
fn dry_run_shows_transitive_dependencies() {
    let (_dir, sync_dir) = fixture_sync_dir(&[
        FixturePkg {
            name: "leaf",
            version: "1.0-1",
            depends: &[],
        },
        FixturePkg {
            name: "top",
            version: "2.0-1",
            depends: &["leaf"],
        },
    ]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "top", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL),
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("top"));
    assert!(out.stdout.contains("leaf"));
    assert!(out.stdout.contains("required by: top"));
    assert!(out.stdout.contains("╭─ Install plan"));
    assert!(out.stdout.contains("│  2 packages · 2 official · 0 AUR"));
    assert!(out.stdout.contains("╰─"));
}

#[test]
fn plan_is_equivalent_to_install_dry_run() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["plan", "leaf"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL),
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Install plan"));
    assert!(out.stdout.contains("leaf"));
}

#[test]
fn package_not_found_anywhere_fails_with_clear_error() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "does-not-exist-anywhere", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(
        !out.status.success(),
        "expected failure, stdout: {}",
        out.stdout
    );
    assert!(
        out.stderr.contains("not found"),
        "expected a clear not-found error, got: {}",
        out.stderr
    );
}

#[test]
fn not_found_suggests_similarly_named_official_packages() {
    // Regression case: `archon install cuttlefish` used to just fail with
    // "not found", even though `cuttlefish-base-git` exists — no exact
    // package is named `cuttlefish` at all, so exact lookup alone can never
    // find it, and the user has no way to discover the real name.
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "cuttlefish-base-git",
        version: "1:r1-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "cuttlefish", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(
        !out.status.success(),
        "expected failure, stdout: {}",
        out.stdout
    );
    assert!(out.stderr.contains("not found"), "got: {}", out.stderr);
    assert!(
        out.stderr.contains("cuttlefish-base-git"),
        "expected a 'did you mean' suggestion naming the real package, got: {}",
        out.stderr
    );
}

#[test]
fn typo_correction_noninteractive_fails_fast_with_suggestions_printed() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "firefox",
        version: "128.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "fierfox", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
            ("ARCHON_LOCAL_DIR", &local_dir),
        ],
    );

    // stdin is null in tests and we didn't force interactive, so it will fail fast.
    assert!(
        !out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr
            .contains("package not found, did you mean: firefox"),
        "got: {}",
        out.stderr
    );
}

#[test]
fn typo_correction_accepts_suggestion_and_proceeds() {
    use common::run_archon_interactive;
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "firefox",
        version: "128.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();

    let out = run_archon_interactive(
        &["install", "fierfox", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
            ("ARCHON_LOCAL_DIR", &local_dir),
        ],
        "\n", // Enter
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("Did you mean"), "got: {}", out.stdout);
    assert!(out.stdout.contains("firefox"), "got: {}", out.stdout);
    assert!(
        out.stdout.contains("Install plan"),
        "must proceed to plan: {}",
        out.stdout
    );
}

#[test]
fn typo_correction_rejects_explicit_cancel() {
    use common::run_archon_interactive;
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "firefox",
        version: "128.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();

    let out = run_archon_interactive(
        &["install", "fierfox", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
            ("ARCHON_LOCAL_DIR", &local_dir),
        ],
        "n\n", // Cancel
    );

    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(130)); // SIGINT/Cancel exit code
    assert!(out.stdout.contains("Cancelled."), "got: {}", out.stdout);
}

#[test]
fn typo_correction_reprompts_on_invalid_selection_then_fails_after_cap() {
    use common::run_archon_interactive;
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "firefox",
        version: "128.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();

    let out = run_archon_interactive(
        &["install", "fierfox", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
            ("ARCHON_LOCAL_DIR", &local_dir),
        ],
        "99\ngarbage\n-1\n", // 3 invalid attempts
    );

    assert!(!out.status.success());
    assert!(out.stdout.contains("Invalid selection"));
    assert!(
        out.stdout.contains("Too many invalid attempts"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn typo_correction_respects_max_depth_when_suggestion_also_not_found() {
    let (_sync, sync) = fixture_sync_dir(&[]);
    let (_cache, cache) = empty_cache_dir();
    let (_local, local) = empty_local_dir();
    let url = common::correction_chain_aur_server();
    let out = common::run_archon_interactive(
        &["install", "fierfox", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", sync.to_str().unwrap()),
            ("ARCHON_CACHE_DIR", cache.to_str().unwrap()),
            ("ARCHON_LOCAL_DIR", local.to_str().unwrap()),
            ("ARCHON_AUR_URL", &url),
        ],
        "\n\n",
    );
    assert!(!out.status.success());
    assert!(
        out.stderr.contains("Recursion depth exceeded"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("fierfox -> firefox -> firefox2"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("panicked"), "{}", out.stderr);
    assert!(!out.stdout.contains("pacman -U"));
}

#[test]
fn empty_sync_database_fixture_is_a_valid_plain_tar() {
    let (_keepalive, sync_dir) = fixture_sync_dir(&[]);
    let db = archon_repo::SyncDb::load(&sync_dir.join("core.db"))
        .expect("empty fixture database is still a valid tar archive");
    assert!(db.is_empty());
}

#[test]
fn arch_typo_correction_excludes_debian_packages_from_mock_apt_lists() {
    use std::fs::File;
    use std::io::Write;
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "dummy",
        version: "1.0",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();
    let (_local, local_dir) = empty_local_dir();
    let local_dir = local_dir.to_string_lossy().into_owned();

    let apt_dir = tempfile::tempdir().unwrap();
    let mut f = File::create(apt_dir.path().join("main_Packages")).unwrap();
    f.write_all(b"Package: ubuntu-restricted-extras\nVersion: 1.0\nArchitecture: amd64\n")
        .unwrap();

    let out = run_archon(
        &["install", "ubuntu-restricted-extres", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
            ("ARCHON_LOCAL_DIR", &local_dir),
            ("ARCHON_APT_LISTS_DIR", apt_dir.path().to_str().unwrap()),
        ],
    );

    assert!(!out.status.success());
    assert!(out.stderr.contains("not found"), "got: {}", out.stderr);
    assert!(
        !out.stderr.contains("ubuntu-restricted-extras"),
        "Arch suggestions must exclude apt candidates: {}",
        out.stderr
    );
    assert!(!out.stderr.contains("panicked"), "got: {}", out.stderr);
}

#[test]
fn search_finds_official_matches_by_substring() {
    let (_dir, sync_dir) = fixture_sync_dir(&[
        FixturePkg {
            name: "firefox",
            version: "128.0-1",
            depends: &[],
        },
        FixturePkg {
            name: "firefox-developer-edition",
            version: "129.0-1",
            depends: &[],
        },
        FixturePkg {
            name: "unrelated",
            version: "1.0-1",
            depends: &[],
        },
    ]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["search", "firefox"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("firefox"));
    assert!(out.stdout.contains("firefox-developer-edition"));
    assert!(!out.stdout.contains("unrelated"));
}

#[test]
fn search_with_no_matches_still_succeeds() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["search", "does-not-exist-anywhere"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", &aur_url),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    assert!(
        out.status.success(),
        "a search with no hits is not an error: {}",
        out.stderr
    );
}

#[test]
fn install_dry_run_on_debian_delegates_to_apt() {
    let (_os, os_release) = fixture_os_release("ID=ubuntu\nID_LIKE=debian\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "firefox", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("Debian/Ubuntu"), "got: {}", out.stdout);
    assert!(
        out.stdout.contains("apt install firefox"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn install_yes_on_fedora_passes_through_assume_yes_flag() {
    let (_os, os_release) = fixture_os_release("ID=fedora\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "firefox", "--dry-run", "--yes"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("dnf install -y firefox"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn install_on_unsupported_distro_fails_naming_what_was_detected() {
    let (_os, os_release) = fixture_os_release("ID=slackware\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "firefox", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        !out.status.success(),
        "expected failure, stdout: {}",
        out.stdout
    );
    assert!(out.stderr.contains("slackware"), "got: {}", out.stderr);
    assert!(
        out.stderr.contains("no supported package manager"),
        "got: {}",
        out.stderr
    );
}

#[test]
fn remove_dry_run_on_fedora_delegates_to_dnf() {
    let (_os, os_release) = fixture_os_release("ID=fedora\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["remove", "firefox", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("Fedora/RHEL"), "got: {}", out.stdout);
    assert!(
        out.stdout.contains("dnf remove firefox"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn install_dry_run_on_nixos_uses_user_profile_without_sudo() {
    let (_os, os_release) = fixture_os_release("ID=nixos\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "firefox", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("NixOS"), "got: {}", out.stdout);
    // Channel-attribute install, and critically NO sudo — nix-env installs
    // into the invoking user's profile; sudo would target root's instead.
    assert!(
        out.stdout.contains("nix-env -iA nixos.firefox"),
        "got: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("sudo nix-env"),
        "nix-env must not run under sudo: {}",
        out.stdout
    );
}

#[test]
fn install_dry_run_on_opensuse_delegates_to_zypper() {
    let (_os, os_release) =
        fixture_os_release("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "vim", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("sudo zypper install vim"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn install_dry_run_on_alpine_delegates_to_apk() {
    let (_os, os_release) = fixture_os_release("ID=alpine\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "vim", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("sudo apk add vim"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn remove_dry_run_on_void_and_gentoo_delegate_natively() {
    let (_os_void, void_release) = fixture_os_release("ID=void\n");
    let void_release = void_release.to_string_lossy().into_owned();
    let out = run_archon(
        &["remove", "vim", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &void_release)],
    );
    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("sudo xbps-remove -R vim"),
        "got: {}",
        out.stdout
    );

    let (_os_gentoo, gentoo_release) = fixture_os_release("ID=gentoo\n");
    let gentoo_release = gentoo_release.to_string_lossy().into_owned();
    let out = run_archon(
        &["remove", "vim", "--dry-run"],
        &[("ARCHON_OS_RELEASE", &gentoo_release)],
    );
    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("sudo emerge --ask --depclean vim"),
        "got: {}",
        out.stdout
    );
}

#[test]
fn install_report_on_non_arch_fails_closed_without_fallback() {
    let (_os, os_release) = fixture_os_release("ID=debian\n");
    let os_release = os_release.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "firefox", "--dry-run", "--report"],
        &[("ARCHON_OS_RELEASE", &os_release)],
    );

    assert!(!out.status.success());
    assert!(
        out.stderr.contains("--report is only available on Arch"),
        "got stderr: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("apt install firefox"),
        "must not silently delegate without monitoring: {}",
        out.stdout
    );
}

#[test]
fn package_names_starting_with_dash_are_rejected_before_native_dispatch() {
    let (_os, os_release) = fixture_os_release("ID=debian\n");
    let os_release = os_release.to_string_lossy().into_owned();

    for args in [
        vec!["install", "--dry-run", "--", "--allow-remove-essential"],
        vec!["remove", "--dry-run", "--", "--purge"],
    ] {
        let out = run_archon(&args, &[("ARCHON_OS_RELEASE", &os_release)]);
        assert!(!out.status.success(), "unexpected success for {args:?}");
        assert!(
            out.stderr
                .contains("package names cannot be empty or start with '-'"),
            "got stderr: {}",
            out.stderr
        );
        assert!(
            !out.stdout.contains("apt install") && !out.stdout.contains("apt remove"),
            "option-like input reached native argv: {}",
            out.stdout
        );
    }
}

#[test]
fn missing_sync_dir_fails_with_actionable_error() {
    let out = run_archon(
        &["install", "leaf", "--dry-run"],
        &[
            ("ARCHON_SYNC_DIR", "/nonexistent/path/archon-test-fixture"),
            ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL),
        ],
    );
    assert!(!out.status.success());
    assert!(
        out.stderr
            .contains("failed to load official repo databases")
    );
}

#[test]
fn report_flag_without_root_fails_before_any_prompt() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg {
        name: "leaf",
        version: "1.0-1",
        depends: &[],
    }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let (_cache, cache_dir) = empty_cache_dir();
    let cache_dir = cache_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "leaf", "--report", "--yes"],
        &[
            ("ARCHON_SYNC_DIR", &sync_dir),
            ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL),
            ("ARCHON_CACHE_DIR", &cache_dir),
        ],
    );

    // This test suite never runs as root, so --report must fail cleanly —
    // and critically, it must fail *before* trying to prompt for anything
    // or touch pacman, matching the "no privileged operation without a
    // clear confirmation step" contract from the README/design notes.
    assert!(!out.status.success());
    assert!(
        out.stderr.contains("root") || out.stderr.contains("sudo"),
        "got: {}",
        out.stderr
    );
    assert!(!out.stdout.contains("sudo pacman"));
}

#[test]
fn build_command_rejects_directory_without_pkgbuild() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_archon(&["build", &dir.path().to_string_lossy()], &[]);
    assert!(!out.status.success());
    assert!(out.stderr.contains("PKGBUILD"));
}

#[test]
fn build_command_rejects_nonexistent_directory() {
    let out = run_archon(&["build", "/nonexistent/archon-test-dir-xyz"], &[]);
    assert!(!out.status.success());
}

#[test]
fn build_help_lists_report_flag() {
    let out = run_archon(&["build", "--help"], &[]);
    assert!(out.status.success());
    assert!(out.stdout.contains("--report"));
}

/// The one test that actually runs a sandboxed build — skipped
/// automatically if `bwrap` isn't on this machine, since that's a real
/// system dependency unit tests elsewhere already document as required.
#[test]
fn build_command_succeeds_on_benign_demo_package() {
    if std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: bwrap not available");
        return;
    }
    if std::process::Command::new("makepkg")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: makepkg not available (required for sandbox test)");
        return;
    }

    let demo_src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../demo/archon-demo-benign");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archon-demo-benign");
    copy_dir(std::path::Path::new(demo_src), &dest);

    let out = run_archon(&["build", &dest.to_string_lossy()], &[]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains(".pkg.tar"));

    let has_pkg_file = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".pkg.tar"));
    assert!(
        has_pkg_file,
        "expected a .pkg.tar* file in {}",
        dest.display()
    );
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dest_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest_path);
        } else {
            std::fs::copy(entry.path(), &dest_path).unwrap();
        }
    }
}
