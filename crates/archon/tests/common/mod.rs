//! Shared helpers for archon's CLI integration tests. Everything here
//! builds fixture data or spawns a mock server so tests never need a real
//! Arch system, live sync databases, or network access to the AUR.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

pub fn archon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_archon")
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
            .append_data(&mut header, format!("{}-{}/desc", pkg.name, pkg.version), desc.as_bytes())
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

/// An AUR URL guaranteed to fail fast (nothing listens on TCP port 1) —
/// for tests where the fixture sync db already satisfies every package,
/// so the AUR is never actually queried, but we still want to prove that
/// if it *were* queried, the test wouldn't hang on a real network call.
pub const UNREACHABLE_AUR_URL: &str = "http://127.0.0.1:1";
