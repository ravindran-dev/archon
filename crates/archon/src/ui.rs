//! Terminal presentation for install plans, confirmation prompts, and
//! build reports. Colors are applied here, not in the library crates
//! (`archon-resolver`, `archon-monitor`) — they shouldn't force ANSI codes
//! on every consumer, only the CLI cares about presentation.

use std::io::Write;

use anyhow::Result;
use archon_monitor::BuildReport;
use archon_resolver::{InstallPlan, PlannedPackage};

use crate::color;

pub fn print_plan(plan: &InstallPlan) {
    println!("{}\n", color::bold("Install plan:"));

    if !plan.official.is_empty() {
        println!("{}", color::cyan(&format!("Official repo packages ({}):", plan.official.len())));
        print_rows(&plan.official);
        println!();
    }

    if !plan.aur.is_empty() {
        println!("{}", color::yellow(&format!("AUR packages ({}), build order:", plan.aur.len())));
        print_rows(&plan.aur);
        println!();
    }

    println!(
        "{}",
        color::bold(&format!(
            "Total: {} package(s) — {} official, {} AUR",
            plan.package_count(),
            plan.official.len(),
            plan.aur.len()
        ))
    );
}

fn print_rows(pkgs: &[PlannedPackage]) {
    let name_width = pkgs.iter().map(|p| p.meta.name.len()).max().unwrap_or(0);
    for p in pkgs {
        let mut line = format!(
            "  {:<name_width$}  {}",
            p.meta.name,
            color::dim(&p.meta.version.to_string()),
            name_width = name_width
        );
        if !p.required_by.is_empty() {
            line.push_str(&color::dim(&format!("  (required by: {})", p.required_by.join(", "))));
        }
        println!("{line}");
    }
}

/// Diff-style build report: a clean report prints green, a flagged one
/// prints its violations in red so they can't be scanned past by accident.
pub fn print_report(report: &BuildReport) {
    println!("\n{}", color::bold(&format!("Build report for {}", report.package)));
    println!("  {} syscall event(s) observed", report.total_events);
    if report.is_clean() {
        println!("  {}", color::green("no policy violations — nothing flagged"));
        return;
    }
    println!("  {}", color::bold_red(&format!("{} event(s) FLAGGED:", report.flagged.len())));
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
    print!("{} ", color::bold(&format!("{prompt} [y/N]")));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
