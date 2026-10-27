//! archon CLI entry point.
//!
//! Safety contract with the user (this runs on a daily-driver machine):
//! - Resolution and planning are always read-only and unprivileged.
//! - Anything that mutates the system (pacman -S / -U) happens only after
//!   an explicit interactive confirmation, and always *through* pacman —
//!   archon never touches pacman's databases itself.
//! - `--dry-run` stops after printing the plan, guaranteed side-effect free.

mod archive;
mod color;
mod commands;
mod correction;
mod handoff;
mod pgp;
mod search;
mod spinner;
mod srcinfo;
mod transaction_state;
mod ui;

use archon_distro::Operation;
use archon_distro::transaction::{TransactionOperation, TransactionRequest};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Clone, Copy, ValueEnum)]
enum TransactionAction {
    Install,
    Remove,
    Upgrade,
    Reinstall,
}

impl From<TransactionAction> for TransactionOperation {
    fn from(value: TransactionAction) -> Self {
        match value {
            TransactionAction::Install => Self::Install,
            TransactionAction::Remove => Self::Remove,
            TransactionAction::Upgrade => Self::Upgrade,
            TransactionAction::Reinstall => Self::Reinstall,
        }
    }
}

#[derive(Subcommand)]
enum TransactionCommand {
    /// Resolve an accurate native plan and save it for review (Fedora/libdnf5).
    Plan {
        #[arg(value_enum)]
        operation: TransactionAction,
        packages: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Revalidate and execute a saved plan; stale or uncertain plans are refused.
    Apply {
        id: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// List durable workflow records for this user.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect a saved plan, workflow status and last error.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Diagnose interruption and explain safe recovery without changing packages.
    Recover {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct MutationOptions {
    /// Print the native command without executing or resolving it.
    #[arg(long)]
    dry_run: bool,
    /// Skip Archon's prompt and use the native assume-yes option where available.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct QueryOptions {
    /// Print the native command without executing it.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser)]
#[command(name = "archon", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare, execute and recover native transactions using durable checkpoints.
    Transaction {
        #[command(subcommand)]
        command: TransactionCommand,
    },
    /// Upgrade native packages (Nix: current user profile; Arch: official repos).
    Upgrade(MutationOptions),
    /// Refresh native repository metadata (Arch: use upgrade instead).
    Refresh(MutationOptions),
    /// Reinstall packages through the native backend.
    Reinstall {
        #[arg(required = true)]
        packages: Vec<String>,
        #[command(flatten)]
        options: MutationOptions,
    },
    /// Show native package information.
    Info {
        #[arg(required = true)]
        packages: Vec<String>,
        #[command(flatten)]
        options: QueryOptions,
    },
    /// List installed packages (Nix: current user profile).
    List {
        /// Emit structured installed inventory where supported (Fedora/openSUSE).
        #[arg(long, conflicts_with = "dry_run")]
        json: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show native repository configuration where supported.
    Repos(QueryOptions),
    /// Check native package dependency/database consistency where supported.
    Check(QueryOptions),
    /// Show native transaction history where supported.
    History(QueryOptions),
    /// Find native packages providing a capability or file pattern.
    Provides {
        #[arg(required = true)]
        patterns: Vec<String>,
        #[command(flatten)]
        options: QueryOptions,
    },
    /// Show implemented operations and scope for the detected backend.
    Capabilities,
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
        /// Resolve and save an accurate native-engine plan (currently Fedora).
        #[arg(long)]
        native: bool,
        #[arg(long, requires = "native")]
        json: bool,
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
    /// Install the root-owned receiver required for signed AUR package handoff.
    /// This copies the companion helper to /usr/local/libexec using sudo.
    SetupHelper,
}

fn main() {
    // Bridges the eBPF monitor's debug!/info! calls (see archon-monitor's
    // EbpfLogger) to stderr. Set RUST_LOG=debug to see them.
    env_logger::init();
    let json_output = std::env::args_os().any(|arg| arg == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if !json_output
                || matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                error.exit();
            }
            println!(
                "{}",
                serde_json::json!({"schema_version": 1, "event": "error", "error": {"code": "invalid_arguments", "message": error.to_string()}})
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = run(cli) {
        if json_output {
            let code = error
                .downcast_ref::<commands::transaction::Failure>()
                .map(|failure| failure.code.as_str())
                .unwrap_or("operation_failed");
            println!(
                "{}",
                serde_json::json!({"schema_version": 1, "event": "error", "error": {"code": code, "message": format!("{error:#}")}})
            );
        } else {
            eprintln!("Error: {error:#}");
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Transaction { command } => match command {
            TransactionCommand::Plan {
                operation,
                packages,
                json,
            } => commands::transaction::prepare(
                TransactionRequest {
                    operation: operation.into(),
                    packages,
                },
                json,
            ),
            TransactionCommand::Apply { id, yes, json } => {
                commands::transaction::apply(&id, yes, json)
            }
            TransactionCommand::List { json } => commands::transaction::list(json),
            TransactionCommand::Show { id, json } => commands::transaction::show(&id, json),
            TransactionCommand::Recover { id, json } => commands::transaction::recover(&id, json),
        },
        Command::Upgrade(options) => {
            commands::native::run(Operation::Upgrade, &[], options.dry_run, options.yes)
        }
        Command::Refresh(options) => {
            commands::native::run(Operation::Refresh, &[], options.dry_run, options.yes)
        }
        Command::Reinstall { packages, options } => commands::native::run(
            Operation::Reinstall,
            &packages,
            options.dry_run,
            options.yes,
        ),
        Command::Info { packages, options } => {
            commands::native::run(Operation::Info, &packages, options.dry_run, false)
        }
        Command::List { json: true, .. } => commands::native::inventory(),
        Command::List { dry_run, .. } => {
            commands::native::run(Operation::List, &[], dry_run, false)
        }
        Command::Repos(options) => {
            commands::native::run(Operation::Repositories, &[], options.dry_run, false)
        }
        Command::Check(options) => {
            commands::native::run(Operation::Check, &[], options.dry_run, false)
        }
        Command::History(options) => {
            commands::native::run(Operation::History, &[], options.dry_run, false)
        }
        Command::Provides { patterns, options } => {
            commands::native::run(Operation::Provides, &patterns, options.dry_run, false)
        }
        Command::Capabilities => commands::native::capabilities(),
        Command::Install {
            packages,
            dry_run,
            yes,
            report,
        } => commands::install::run(&packages, dry_run, yes, report),
        Command::Plan {
            packages,
            native: true,
            json,
        } => commands::transaction::prepare(
            TransactionRequest {
                operation: TransactionOperation::Install,
                packages,
            },
            json,
        ),
        Command::Plan { packages, .. } => commands::install::run(&packages, true, false, false),
        Command::Remove {
            packages,
            dry_run,
            yes,
        } => commands::remove::run(&packages, dry_run, yes),
        Command::Build { dir, report } => {
            commands::build::run(&commands::build::parse_dir(&dir), report)
        }
        Command::Search { term, all } => commands::search::run(&term, all),
        Command::SetupHelper => {
            let executable = std::env::current_exe()?;
            let source = executable
                .parent()
                .context("cannot locate archon executable directory")?
                .join("archon-install-helper");
            handoff::install_helper(&source)?;
            println!("Installed trusted package helper at {}", handoff::HELPER);
            Ok(())
        }
    }
}

use anyhow::Context;
