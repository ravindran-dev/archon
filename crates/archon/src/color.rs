//! Minimal ANSI color helpers. No dependency needed for a handful of SGR
//! codes; respects `NO_COLOR` (https://no-color.org) and disables itself
//! when stdout isn't a terminal (piped output, `archon ... | tee log`).

use std::io::IsTerminal;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
    })
}

fn wrap(s: &str, code: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn red(s: &str) -> String {
    wrap(s, "31")
}
pub fn green(s: &str) -> String {
    wrap(s, "32")
}
pub fn yellow(s: &str) -> String {
    wrap(s, "33")
}
pub fn cyan(s: &str) -> String {
    wrap(s, "36")
}
pub fn bold(s: &str) -> String {
    wrap(s, "1")
}
pub fn dim(s: &str) -> String {
    wrap(s, "2")
}
pub fn bold_red(s: &str) -> String {
    wrap(s, "1;31")
}
pub fn bold_green(s: &str) -> String {
    wrap(s, "1;32")
}
