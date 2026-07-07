//! The build report: what a sandboxed build actually did, and whether
//! anything was flagged. This is what `archon install --report` shows
//! before the user is asked to confirm installing the built package.

use archon_monitor_common::SyscallEvent;

use crate::policy::FlagReason;

pub struct FlaggedEvent {
    pub event: SyscallEvent,
    pub reason: FlagReason,
}

pub struct BuildReport {
    pub package: String,
    pub total_events: usize,
    pub flagged: Vec<FlaggedEvent>,
}

impl BuildReport {
    pub fn is_clean(&self) -> bool {
        self.flagged.is_empty()
    }
}

impl std::fmt::Display for BuildReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Build report for {}", self.package)?;
        writeln!(f, "  {} syscall event(s) observed", self.total_events)?;
        if self.is_clean() {
            writeln!(f, "  no policy violations — nothing flagged")?;
            return Ok(());
        }
        writeln!(f, "  {} event(s) FLAGGED:", self.flagged.len())?;
        for flagged in &self.flagged {
            writeln!(
                f,
                "    [{}] pid={} comm={} — {}",
                flagged.event.timestamp_ns,
                flagged.event.pid,
                flagged.event.comm_str(),
                flagged.reason
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archon_monitor_common::EventKind;

    #[test]
    fn clean_report_says_so() {
        let report = BuildReport {
            package: "tty-clock".into(),
            total_events: 42,
            flagged: vec![],
        };
        assert!(report.is_clean());
        let text = report.to_string();
        assert!(text.contains("42 syscall"));
        assert!(text.contains("nothing flagged"));
    }

    #[test]
    fn flagged_report_lists_reasons() {
        let event = SyscallEvent::zeroed(EventKind::Exec);
        let report = BuildReport {
            package: "evil-pkg".into(),
            total_events: 10,
            flagged: vec![FlaggedEvent {
                event,
                reason: FlagReason::UnexpectedExec { path: "/tmp/payload".into() },
            }],
        };
        assert!(!report.is_clean());
        let text = report.to_string();
        assert!(text.contains("FLAGGED"));
        assert!(text.contains("/tmp/payload"));
    }
}
