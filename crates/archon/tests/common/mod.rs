//! Shared helpers for archon's CLI integration tests. Everything here
//! builds fixture data or spawns a mock server so tests never need a real
//! Arch system, live sync databases, or network access to the AUR.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn archon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_archon")
}

/// A fixed `/etc/os-release`-style fixture declaring Arch — every existing
/// CLI test exercises archon's Arch-specific pipeline (resolver, sandbox,
/// AUR), so `run_archon` defaults every test to this unless the test
/// itself overrides `ARCHON_OS_RELEASE`, keeping the whole suite hermetic
/// against whatever distro actually runs it. A `OnceLock`-backed shared
/// file rather than per-test tempfiles: the content never varies, so one
/// file for the whole test binary is simpler and avoids creating hundreds
/// of near-identical temp files across a full test run.
fn arch_os_release_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("archon-test-os-release-{}", std::process::id()));
        std::fs::write(&path, "ID=arch\n").expect("write fixture os-release");
        path
    })
}

/// One package for a fixture sync database.
pub struct FixturePkg {
    pub name: &'static str,
    pub version: &'static str,
    pub depends: &'static [&'static str],
}

/// Builds a `core.db` under a fresh temp dir containing the given
/// packages, mirroring the real tarball format `archon-repo` reads
/// (`*/desc` entries, `%FIELD%` sections). Returns the TempDir (keep it
/// alive for the fixture's lifetime) and the directory path to pass as
/// `ARCHON_SYNC_DIR`.
pub fn fixture_sync_dir(pkgs: &[FixturePkg]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db_path = dir.path().join("core.db");

    let mut builder = tar::Builder::new(Vec::new());
    for pkg in pkgs {
        let mut desc = format!("%NAME%\n{}\n\n%VERSION%\n{}\n\n", pkg.name, pkg.version);
        if !pkg.depends.is_empty() {
            desc.push_str("%DEPENDS%\n");
            for d in pkg.depends {
                desc.push_str(d);
                desc.push('\n');
            }
            desc.push('\n');
        }
        let mut header = tar::Header::new_ustar();
        header.set_size(desc.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{}-{}/desc", pkg.name, pkg.version),
                desc.as_bytes(),
            )
            .expect("append fixture package");
    }
    let bytes = builder.into_inner().expect("finish tar");
    std::fs::write(&db_path, bytes).expect("write fixture db");

    let path = dir.path().to_path_buf();
    (dir, path)
}

/// A minimal AUR RPC v5 mock: always responds with an empty result set,
/// for tests that need the AUR provider reachable-but-empty (a genuine
/// "not found anywhere" case) rather than a connection failure.
pub fn empty_aur_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock AUR server");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = r#"{"type":"multiinfo","resultcount":0,"results":[],"version":5}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
}

pub struct Output {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the compiled `archon` binary with the given args and env
/// overrides, returning its captured output. Never inherits stdin (a
/// `[y/N]` prompt reading EOF gets an empty line, i.e. "no" — safe default
/// for tests that don't expect to be asked anything anyway because they
/// pass `--dry-run` or expect an early failure).
pub fn run_archon(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(archon_bin());
    cmd.args(args).stdin(std::process::Stdio::null());
    cmd.env("ARCHON_OS_RELEASE", arch_os_release_path());
    cmd.env("ARCHON_APT_LISTS_DIR", "/tmp/archon-empty-apt-lists");
    std::fs::create_dir_all("/tmp/archon-empty-apt-lists").ok();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to execute archon binary");
    Output {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

pub fn run_archon_interactive(args: &[&str], envs: &[(&str, &str)], input: &str) -> Output {
    let mut cmd = Command::new(archon_bin());
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.env("ARCHON_OS_RELEASE", arch_os_release_path());
    cmd.env("ARCHON_FORCE_INTERACTIVE", "1");
    cmd.env("ARCHON_APT_LISTS_DIR", "/tmp/archon-empty-apt-lists");
    std::fs::create_dir_all("/tmp/archon-empty-apt-lists").ok();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn archon");
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(input.as_bytes())
            .expect("failed to write to stdin");
    }
    let output = child.wait_with_output().expect("failed to wait for archon");
    Output {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// An empty local-install-db directory, so plan-display tests don't
/// implicitly depend on whatever happens to be installed on the machine
/// running them (via `ARCHON_LOCAL_DIR`) — every fixture package shows as
/// "new", deterministically, regardless of the host system's state.
pub fn empty_local_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// A local-install-db directory populated with the given packages —
/// plain `<pkg>-<ver>/desc` directories (not tarballs; that's the real
/// `/var/lib/pacman/local` layout, distinct from the sync db's tar
/// format), for tests that need `archon remove` to see something as
/// actually installed.
pub fn fixture_local_dir(pkgs: &[FixturePkg]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    for pkg in pkgs {
        let pkg_dir = dir.path().join(format!("{}-{}", pkg.name, pkg.version));
        std::fs::create_dir_all(&pkg_dir).expect("create fixture pkg dir");
        std::fs::write(
            pkg_dir.join("desc"),
            format!(
                "%NAME%\n{}\n\n%VERSION%\n{}\n\n%BASE%\n{}\n",
                pkg.name, pkg.version, pkg.name
            ),
        )
        .expect("write fixture desc");
    }
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// A fresh, empty cache directory (via `ARCHON_CACHE_DIR`) — each test
/// gets its own, so parallel test runs never share (or wait on) a cache
/// file, and none of them touch the real user's `~/.cache/archon`.
pub fn empty_cache_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// An AUR URL guaranteed to fail fast (nothing listens on TCP port 1) —
/// for tests where the fixture sync db already satisfies every package,
/// so the AUR is never actually queried, but we still want to prove that
/// if it *were* queried, the test wouldn't hang on a real network call.
pub const UNREACHABLE_AUR_URL: &str = "http://127.0.0.1:1";

/// An `/etc/os-release`-style fixture with the given raw content, for
/// tests exercising the non-Arch delegation path — pass via
/// `ARCHON_OS_RELEASE`, overriding `run_archon`'s own Arch-by-default.
pub fn fixture_os_release(content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("os-release");
    std::fs::write(&path, content).expect("write fixture os-release");
    (dir, path)
}
