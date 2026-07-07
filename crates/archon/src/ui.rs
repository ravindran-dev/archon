//! Terminal presentation for install plans and confirmation prompts.
//! Phase 1 keeps this plain (no color) — Phase 4 adds colored diff-style
//! output for flagged security events.

use std::io::Write;

use anyhow::Result;
use archon_resolver::{InstallPlan, PlannedPackage};

pub fn print_plan(plan: &InstallPlan) {
    println!("Install plan:\n");

    if !plan.official.is_empty() {
        println!("Official repo packages ({}):", plan.official.len());
        print_rows(&plan.official);
        println!();
    }

    if !plan.aur.is_empty() {
        println!("AUR packages ({}), build order:", plan.aur.len());
        print_rows(&plan.aur);
        println!();
    }

    println!(
        "Total: {} package(s) — {} official, {} AUR",
        plan.package_count(),
        plan.official.len(),
        plan.aur.len()
    );
}

fn print_rows(pkgs: &[PlannedPackage]) {
    let name_width = pkgs.iter().map(|p| p.meta.name.len()).max().unwrap_or(0);
    for p in pkgs {
        let mut line = format!(
            "  {:<name_width$}  {}",
            p.meta.name,
            p.meta.version,
            name_width = name_width
        );
        if !p.required_by.is_empty() {
            line.push_str(&format!("  (required by: {})", p.required_by.join(", ")));
        }
        println!("{line}");
    }
}

/// Interactive `[y/N]` prompt. Anything but an explicit y/yes is a no —
/// this runs on the user's daily-driver machine, so ambiguous input must
/// never be treated as consent.
pub fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
