//! CLI integration tests: exercise the compiled `archon` binary as a black
//! box. Every test here runs against a fixture sync database (built
//! in-memory, see `common::fixture_sync_dir`) via `$ARCHON_SYNC_DIR` —
//! none of them touch the real system's pacman databases, the real AUR,
//! or need root. The few things that genuinely can't be tested this way
//! (the eBPF monitor, real sandboxed builds) are covered by the unit
//! tests in `archon-sandbox`/`archon-monitor` and by manual live testing —
//! see the README's Status section.

mod common;

use common::{FixturePkg, UNREACHABLE_AUR_URL, empty_aur_server, fixture_sync_dir, run_archon};

#[test]
fn help_lists_all_subcommands() {
    let out = run_archon(&["--help"], &[]);
    assert!(out.status.success());
    assert!(out.stdout.contains("install"));
    assert!(out.stdout.contains("plan"));
    assert!(out.stdout.contains("build"));
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
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg { name: "leaf", version: "1.0-1", depends: &[] }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "leaf", "--dry-run"],
        &[("ARCHON_SYNC_DIR", &sync_dir), ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL)],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Install plan"));
    assert!(out.stdout.contains("leaf"));
    assert!(out.stdout.contains("Total:"));
    assert!(
        !out.stdout.contains("sudo pacman"),
        "dry-run must never reach the pacman invocation: {}",
        out.stdout
    );
}

#[test]
fn dry_run_shows_transitive_dependencies() {
    let (_dir, sync_dir) = fixture_sync_dir(&[
        FixturePkg { name: "leaf", version: "1.0-1", depends: &[] },
        FixturePkg { name: "top", version: "2.0-1", depends: &["leaf"] },
    ]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "top", "--dry-run"],
        &[("ARCHON_SYNC_DIR", &sync_dir), ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL)],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("top"));
    assert!(out.stdout.contains("leaf"));
    assert!(out.stdout.contains("required by: top"));
}

#[test]
fn plan_is_equivalent_to_install_dry_run() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg { name: "leaf", version: "1.0-1", depends: &[] }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["plan", "leaf"],
        &[("ARCHON_SYNC_DIR", &sync_dir), ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL)],
    );

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Install plan"));
    assert!(out.stdout.contains("leaf"));
}

#[test]
fn package_not_found_anywhere_fails_with_clear_error() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg { name: "leaf", version: "1.0-1", depends: &[] }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();
    let aur_url = empty_aur_server();

    let out = run_archon(
        &["install", "does-not-exist-anywhere", "--dry-run"],
        &[("ARCHON_SYNC_DIR", &sync_dir), ("ARCHON_AUR_URL", &aur_url)],
    );

    assert!(!out.status.success(), "expected failure, stdout: {}", out.stdout);
    assert!(
        out.stderr.contains("not found"),
        "expected a clear not-found error, got: {}",
        out.stderr
    );
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
    assert!(out.stderr.contains("failed to load official repo databases"));
}

#[test]
fn report_flag_without_root_fails_before_any_prompt() {
    let (_dir, sync_dir) = fixture_sync_dir(&[FixturePkg { name: "leaf", version: "1.0-1", depends: &[] }]);
    let sync_dir = sync_dir.to_string_lossy().into_owned();

    let out = run_archon(
        &["install", "leaf", "--report", "--yes"],
        &[("ARCHON_SYNC_DIR", &sync_dir), ("ARCHON_AUR_URL", UNREACHABLE_AUR_URL)],
    );

    // This test suite never runs as root, so --report must fail cleanly —
    // and critically, it must fail *before* trying to prompt for anything
    // or touch pacman, matching the "no privileged operation without a
    // clear confirmation step" contract from the README/design notes.
    assert!(!out.status.success());
    assert!(out.stderr.contains("root") || out.stderr.contains("sudo"), "got: {}", out.stderr);
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
    if std::process::Command::new("bwrap").arg("--version").output().is_err() {
        eprintln!("skipping: bwrap not available");
        return;
    }

    let demo_src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../demo/archon-demo-benign");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archon-demo-benign");
    copy_dir(std::path::Path::new(demo_src), &dest);

    let out = run_archon(&["build", &dest.to_string_lossy()], &[]);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", out.stdout, out.stderr);
    assert!(out.stdout.contains("produced:"));

    let has_pkg_file = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".pkg.tar"));
    assert!(has_pkg_file, "expected a .pkg.tar* file in {}", dest.display());
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
