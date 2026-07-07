//! Manual test harness: `cargo run --example build_pkg -- <pkg-dir>`.
//! Runs the two-stage sandboxed build against a directory that already
//! contains a PKGBUILD (e.g. a fresh AUR git clone) and reports the
//! resulting package files. Never installs anything.

use std::path::PathBuf;

use archon_sandbox::SandboxRunner;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: build_pkg <path-to-pkgbuild-dir>");
    let dir = PathBuf::from(dir);

    if !SandboxRunner::bwrap_available() {
        eprintln!("bwrap not found in PATH");
        std::process::exit(1);
    }

    println!("==> building {} in sandbox", dir.display());
    let runner = SandboxRunner::new(&dir);
    if let Err(e) = runner.build() {
        eprintln!("build failed: {e}");
        std::process::exit(1);
    }
    println!("==> build succeeded");

    let entries = std::fs::read_dir(&dir).expect("read pkg dir");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".pkg.tar.zst") || name.ends_with(".pkg.tar.xz") {
            println!("==> produced package: {}", entry.path().display());
        }
    }
}
