//! archon CLI entry point.
//!
//! Safety contract with the user (this runs on a daily-driver machine):
//! - Resolution and planning are always read-only and unprivileged.
//! - Anything that mutates the system (pacman -S / -U) happens only after
//!   an explicit interactive confirmation, and always *through* pacman —
//!   archon never touches pacman's databases itself.
//! - `--dry-run` stops after printing the plan, guaranteed side-effect free.

mod color;
mod commands;
mod pgp;
mod search;
mod spinner;
mod srcinfo;
mod ui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "archon", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve, show the install plan, confirm, then install.
    Install {
        /// Packages to install (official repo or AUR).
        #[arg(required = true)]
        packages: Vec<String>,
        /// Print the resolved install plan and exit without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt (still refuses on resolution errors).
        #[arg(long)]
        yes: bool,
        /// Monitor AUR builds with eBPF and show a report before offering
        /// to install. Requires root — re-run the whole command under
        /// `sudo`; archon drops back to your normal user to run the
        /// actual build (`makepkg` refuses to run as root regardless).
        #[arg(long)]
        report: bool,
    },
    /// Resolve and print the install plan (alias for `install --dry-run`).
    Plan {
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Remove installed packages (delegates to `pacman -Rs`, which also
    /// removes any dependency no longer required by anything else).
    Remove {
        /// Packages to remove.
        #[arg(required = true)]
        packages: Vec<String>,
        /// Print what would be removed and exit without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Build a local PKGBUILD directory in the sandbox (never installs it).
    /// Use `--report` to build under eBPF monitoring — the intended way to
    /// exercise the demo packages under `demo/`.
    Build {
        /// Directory containing a PKGBUILD (e.g. demo/archon-demo-evil).
        dir: String,
        /// Build under eBPF monitoring and print a security report.
        /// Requires root (re-run under `sudo`); archon drops privileges
        /// for the build itself.
        #[arg(long)]
        report: bool,
    },
    /// Search official repos and the AUR for packages by (partial) name —
    /// use this when you're not sure of the exact package name. Results
    /// are ranked by AUR popularity (a proxy for what users actually
    /// install), exact matches first; the top 15 are shown by default.
    Search {
        term: String,
        /// Show every match instead of the top 15.
        #[arg(long)]
        all: bool,
    },
}

fn main() -> anyhow::Result<()> {
    // Bridges the eBPF monitor's debug!/info! calls (see archon-monitor's
    // EbpfLogger) to stderr. Set RUST_LOG=debug to see them.
    env_logger::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Install {
            packages,
            dry_run,
            yes,
            report,
        } => commands::install::run(&packages, dry_run, yes, report),
        Command::Plan { packages } => commands::install::run(&packages, true, false, false),
        Command::Remove { packages, dry_run, yes } => commands::remove::run(&packages, dry_run, yes),
        Command::Build { dir, report } => {
            commands::build::run(&commands::build::parse_dir(&dir), report)
        }
        Command::Search { term, all } => commands::search::run(&term, all),
    }
}
