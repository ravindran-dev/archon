//! Read-only official-repository benchmark. Does not fetch AUR or install.
use archon_repo::{DEFAULT_REPO_ORDER, LocalDb, OfficialRepos};
use archon_resolver::Resolver;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let repos = OfficialRepos::load(Path::new("/var/lib/pacman/sync"), DEFAULT_REPO_ORDER)?;
    let installed: HashSet<String> = LocalDb::load(Path::new("/var/lib/pacman/local"))?
        .names()
        .map(str::to_string)
        .collect();
    println!(
        "Official DB + local DB load: {:.2} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    println!("Official-only resolver; AUR/network/build/install are not exercised.");
    println!("PACKAGE\tPACKAGES\tRESOLVE_MS\tRESULT");
    for target in [
        "vlc",
        "mpv",
        "ffmpeg",
        "libreoffice-fresh",
        "nmap",
        "wireshark-qt",
        "metasploit",
        "sqlmap",
        "hashcat",
        "john",
        "hydra",
        "aircrack-ng",
        "google-chrome",
    ] {
        let start = Instant::now();
        let result = Resolver::new(vec![&repos])
            .prefer_installed(&installed)
            .resolve(&[target.into()]);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(plan) => println!("{target}\t{}\t{elapsed:.2}\tPASS", plan.package_count()),
            Err(error) => {
                println!("{target}\t-\t{elapsed:.2}\tUNRESOLVED (official-only): {error}")
            }
        }
    }
    Ok(())
}
