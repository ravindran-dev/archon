//! Terminal presentation for install plans, confirmation prompts, and
//! build reports. Colors are applied here, not in the library crates
//! (`archon-resolver`, `archon-monitor`) — they shouldn't force ANSI codes
//! on every consumer, only the CLI cares about presentation.

use std::io::Write;

use anyhow::Result;
use archon_monitor::BuildReport;
use archon_repo::LocalDb;
use archon_resolver::{InstallPlan, PlannedPackage};

use crate::color;

/// Above this many official packages, per-package detail (version,
/// required-by) stops being "relevant detail" and starts being a wall of
/// text — a `firefox`-sized 260-package tree would otherwise print
/// hundreds of `required by: glibc, systemd, ...` lines. Below it (the
/// common case — most installs are a handful of packages), full detail is
/// exactly what's worth showing.
const DETAIL_THRESHOLD: usize = 12;

/// Above this many, even a bare wrapped name list is too much to scan —
/// collapse to a count and a sample instead.
const SAMPLE_THRESHOLD: usize = 60;
const SAMPLE_SIZE: usize = 16;

/// AUR packages always get a full line each (name, version, and why —
/// that's the part worth reading closely before a build runs unsandboxed
/// code) regardless of tree size, since there are rarely more than one or
/// two of them and they're the security-relevant part of the plan.
///
/// `local` (pacman's install database, best-effort — `None` if it
/// couldn't be read) filters out anything already installed at a
/// satisfying version, the same bar `pacman -S --needed` itself uses.
/// Without this, a library-heavy package's plan shows its full
/// theoretical dependency closure regardless of what's actually on the
/// system — for something like `okular` that's 424 entries when the real
/// answer might be "4 new, 420 already installed".
pub fn print_plan(plan: &InstallPlan, local: Option<&LocalDb>) {
    let total = plan.package_count();
    println!("{} {}", color::cyan("╭─"), color::bold("Install plan"));
    println!(
        "{}  {} package{} · {} official · {} AUR",
        color::cyan("│"),
        total,
        if total == 1 { "" } else { "s" },
        plan.official.len(),
        plan.aur.len()
    );
    println!("{}", color::cyan("│"));

    let is_satisfied =
        |p: &&PlannedPackage| local.is_some_and(|db| db.satisfies(&p.meta.name, &p.meta.version));

    if !plan.aur.is_empty() {
        let (already, new): (Vec<&PlannedPackage>, Vec<&PlannedPackage>) =
            plan.aur.iter().partition(is_satisfied);
        println!("{}  {}", color::cyan("│"), color::yellow("AUR"));
        for p in &new {
            let mut line = format!(
                "{}    {}  {}",
                color::cyan("│"),
                p.meta.name,
                color::dim(&p.meta.version.to_string())
            );
            if !p.required_by.is_empty() {
                line.push_str(&color::dim(&format!(
                    "  (needs: {})",
                    p.required_by.join(", ")
                )));
            }
            println!("{line}");
        }
        if !already.is_empty() {
            let names: Vec<&str> = already.iter().map(|p| p.meta.name.as_str()).collect();
            println!(
                "{}    {}",
                color::cyan("│"),
                color::dim(&format!("already installed: {}", names.join(", ")))
            );
        }
        println!("{}", color::cyan("│"));
    }

    if !plan.official.is_empty() {
        let (already, new): (Vec<&PlannedPackage>, Vec<&PlannedPackage>) =
            plan.official.iter().partition(is_satisfied);
        if new.is_empty() {
            println!(
                "{}  {}",
                color::cyan("│"),
                color::green(&format!(
                    "✓ Official · all {} already up to date",
                    plan.official.len()
                )),
            );
        } else {
            let suffix = if already.is_empty() {
                String::new()
            } else {
                format!(", {} already current", already.len())
            };
            println!(
                "{}  {}",
                color::cyan("│"),
                color::cyan(&format!("Official · {} new{suffix}", new.len()))
            );
            if new.len() <= DETAIL_THRESHOLD {
                print_detailed(&new);
            } else if new.len() <= SAMPLE_THRESHOLD {
                let names: Vec<&str> = new.iter().map(|p| p.meta.name.as_str()).collect();
                print_wrapped(&names);
            } else {
                let names: Vec<&str> = new
                    .iter()
                    .take(SAMPLE_SIZE)
                    .map(|p| p.meta.name.as_str())
                    .collect();
                print_wrapped(&names);
                println!(
                    "{}    {}",
                    color::cyan("│"),
                    color::dim(&format!("… and {} more", new.len() - SAMPLE_SIZE))
                );
            }
        }
    }
    println!("{}", color::cyan("╰─"));
}

/// One line per package — name, version, and (if it's a transitive
/// dependency, not a direct target) what pulled it in.
fn print_detailed(pkgs: &[&PlannedPackage]) {
    let name_width = pkgs.iter().map(|p| p.meta.name.len()).max().unwrap_or(0);
    for p in pkgs {
        let mut line = format!(
            "{}    {:<name_width$}  {}",
            color::cyan("│"),
            p.meta.name,
            color::dim(&p.meta.version.to_string()),
            name_width = name_width
        );
        if !p.required_by.is_empty() {
            line.push_str(&color::dim(&format!(
                "  (required by: {})",
                p.required_by.join(", ")
            )));
        }
        println!("{line}");
    }
}

/// Word-wraps `names` (comma-separated) to a fixed width, indented and
/// dimmed — the plan's official-package list uses this to stay compact
/// regardless of how large the dependency tree is.
fn print_wrapped(names: &[&str]) {
    const WIDTH: usize = 92;
    const INDENT: &str = "    ";
    let mut line = String::from(INDENT);
    for (i, name) in names.iter().enumerate() {
        let piece = if i + 1 == names.len() {
            name.to_string()
        } else {
            format!("{name}, ")
        };
        if line.len() > INDENT.len() && line.len() + piece.len() > WIDTH {
            println!("{}{}", color::cyan("│"), color::dim(&line));
            line = String::from(INDENT);
        }
        line.push_str(&piece);
    }
    if line.len() > INDENT.len() {
        println!("{}{}", color::cyan("│"), color::dim(&line));
    }
}

/// Diff-style build report: a clean report prints green, a flagged one
/// prints its violations in red so they can't be scanned past by accident.
pub fn print_report(report: &BuildReport) {
    println!(
        "\n{}",
        color::bold(&format!("Build report for {}", report.package))
    );
    println!("  {} syscall event(s) observed", report.total_events);
    if report.is_clean() {
        println!(
            "  {}",
            color::green("no policy violations — nothing flagged")
        );
        return;
    }
    println!(
        "  {}",
        color::bold_red(&format!("{} event(s) FLAGGED:", report.flagged.len()))
    );
    for flagged in &report.flagged {
        println!(
            "    {} pid={} comm={} — {}",
            color::red("[!]"),
            flagged.event.pid,
            flagged.event.comm_str(),
            color::red(&flagged.reason.to_string())
        );
    }
}

/// Interactive `[y/N]` prompt. Anything but an explicit y/yes is a no —
/// this runs on the user's daily-driver machine, so ambiguous input must
/// never be treated as consent.
pub fn confirm(prompt: &str) -> Result<bool> {
    print!("{} {} [y/N] ", color::cyan("◆"), color::bold(prompt.trim()));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
